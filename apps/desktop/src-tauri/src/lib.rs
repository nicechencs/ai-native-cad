#![forbid(unsafe_code)]

use cad_kernel_occt::{GeometryKernel, default_brep_kernel};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::Manager;

#[derive(Clone)]
pub struct DesktopWorkspace {
    configured_root: PathBuf,
    root: PathBuf,
    kernel: Arc<dyn GeometryKernel + Send + Sync>,
}

impl DesktopWorkspace {
    fn with_kernel(
        root: PathBuf,
        kernel: Arc<dyn GeometryKernel + Send + Sync>,
    ) -> Result<Self, std::io::Error> {
        let configured_root = std::path::absolute(root)?;
        fs::create_dir_all(&configured_root)?;
        Ok(Self {
            root: fs::canonicalize(&configured_root)?,
            configured_root,
            kernel,
        })
    }

    fn root_for(&self, requested: Option<&str>) -> Result<&Path, String> {
        // A webview can identify the workspace selected by the native host, not select one.
        // Avoid canonicalizing untrusted IPC paths (which could themselves access remote shares).
        if requested.is_some_and(|path| {
            let path = Path::new(path);
            path != self.root && path != self.configured_root
        }) {
            return Err("Workspace is not authorized by the desktop host".into());
        }
        if fs::canonicalize(&self.root).ok().as_ref() != Some(&self.root) {
            return Err("The desktop workspace is no longer available".into());
        }
        Ok(&self.root)
    }

    pub(crate) fn authorized_root(&self, requested: Option<&str>) -> Result<&Path, String> {
        self.root_for(requested)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub runtime: &'static str,
    pub engine_version: &'static str,
    pub occt_available: bool,
    pub wgpu_backend: &'static str,
    pub offline_ready: bool,
    pub kernel_backend: String,
    pub kernel_authoritative: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompileKclRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub file_name: String,
    pub source: String,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub file_name: String,
    pub source: String,
    pub workspace_root: Option<String>,
    pub out_path: String,
    pub include_pmi: Option<bool>,
    pub views: Option<Vec<String>>,
    #[serde(default)]
    pub allow_non_authoritative_preview: Option<bool>,
}

/// Protocol string shared with the web compile abort path. Not an R-code.
/// STEP, DXF, drawing SVG, and 3MF publication all return this when a trace is
/// cancelled before the target file is replaced.
pub(crate) const EXPORT_CLIENT_ABORTED: &str = "CLIENT_ABORTED";

/// Cancel or discard one desktop export publication. Fields stay snake_case,
/// matching `ExportRequest`; the only invoke argument is `request`.
/// STEP, DXF, and drawing SVG share this payload.
#[derive(Debug, Clone, Deserialize)]
pub struct ExportControlRequest {
    pub trace_id: String,
    #[serde(default)]
    pub out_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub format: &'static str,
    pub out_path: String,
    pub bytes_written: u64,
    pub pmi_entity_count: usize,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoritative: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_backend: Option<String>,
}


fn is_authoritative_kernel_backend(name: &str) -> bool {
    // Evaluator reports live OCCT solids as `occt-ffi`; the kernel's backend_name is `occt`.
    matches!(name, "occt" | "occt-ffi")
}

fn engine_status_from_runtime() -> EngineStatus {
    #[cfg(feature = "occt-ffi")]
    let occt_available = cad_kernel_occt::OcctKernel::is_available();
    #[cfg(not(feature = "occt-ffi"))]
    let occt_available = false;
    let kernel_backend = if occt_available {
        "occt".to_string()
    } else {
        "rust-brep-mvp".to_string()
    };
    let fallback_reason = if occt_available {
        None
    } else if cfg!(feature = "occt-ffi") {
        Some("OCCT 桥接库未就绪或 ABI 握手失败，已回退到解析预览内核。".to_string())
    } else {
        Some("此构建未启用 occt-ffi，当前为解析预览内核。".to_string())
    };
    EngineStatus {
        runtime: "tauri-v2",
        engine_version: env!("CARGO_PKG_VERSION"),
        occt_available,
        wgpu_backend: "wgpu-native",
        offline_ready: true,
        kernel_backend,
        kernel_authoritative: occt_available,
        fallback_reason,
    }
}

pub mod ai_bridge;
pub mod live_attach;
mod observe;
pub mod private_fs;
pub mod project_commands;
pub mod session_commands;

const MAX_ASSET_BYTES: usize = 32 * 1024 * 1024;

fn normalized_asset_name(name: &str) -> Option<String> {
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty()
        || stem.len() > 96
        || !stem.as_bytes()[0].is_ascii_alphanumeric()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || !extension.is_ascii()
    {
        return None;
    }
    let stem = stem.to_ascii_lowercase();
    let extension = extension.to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "step" | "stp" | "brep" | "iges" | "igs" | "stl" | "3mf" | "obj" | "gltf" | "glb"
    ) || matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return None;
    }
    Some(format!("{stem}.{extension}"))
}

fn describe_asset(name: &str, size: u64) -> serde_json::Value {
    let extension = name.rsplit_once('.').expect("validated asset name").1;
    let format = match extension {
        "stp" => "step",
        "igs" => "iges",
        "glb" => "gltf",
        _ => extension,
    };
    let exact = matches!(format, "step" | "brep" | "iges");
    let path = format!("assets/{name}");
    let expression = format!(
        "import::{}(path = \"{path}\")",
        if exact { "step" } else { "mesh" }
    );
    let source = format!("part ImportedAsset() -> Solid {{\n    return {expression}\n}}\n");
    serde_json::json!({"name": name, "path": path, "sizeBytes": size, "format": format, "expression": expression, "source": source})
}

fn workspace_asset_directory(workspace: &DesktopWorkspace) -> Result<PathBuf, String> {
    let dir = workspace.root_for(None)?.join("assets");
    if let Err(err) = fs::create_dir(&dir)
        && err.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err("Could not create the workspace assets directory".into());
    }
    let info = fs::symlink_metadata(&dir)
        .map_err(|_| "Could not access the workspace assets directory")?;
    if !info.is_dir()
        || info.file_type().is_symlink()
        || fs::canonicalize(&dir).ok().as_ref() != Some(&dir)
    {
        return Err(
            "The workspace assets directory must be a real directory inside the workspace".into(),
        );
    }
    Ok(dir)
}

pub mod commands {
    use super::{
        CompileKclRequest, DesktopWorkspace, EXPORT_CLIENT_ABORTED, EngineStatus,
        ExportControlRequest, ExportRequest, ExportResult, MAX_ASSET_BYTES, describe_asset,
        engine_status_from_runtime, is_authoritative_kernel_backend, normalized_asset_name,
        workspace_asset_directory,
    };
    use cad_evaluator::{EvaluationOutput, FsAssetResolver, evaluate_graph_source_output};
    use cad_hir::{Diagnostic, Severity};
    use cad_kcl_bridge::{
        EvalEngineMode, EvalPath, bridge_error_diagnostic, diagnostic_error,
        inspect_graph_eval_readiness,
    };
    use cad_kernel_occt::{BBox3, CheckReport, GeometryKernel, SolidShape};
    use cad_observe::tracing;
    use cad_render::{
        CompactScene, DrawingOptions, EditHandle, HoleOffsetHandle, SceneMeshPayload,
        build_scene_mesh, encode_cms2_envelope, render_dxf, try_render_drawing_svg,
    };
    use serde::Serialize;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    use tauri::State;

    struct UnifiedEvalResult {
        output: EvaluationOutput,
        eval_path: EvalPath,
        fallback_reasons: Vec<String>,
    }

    fn empty_error_output(
        part_name: impl Into<String>,
        diagnostics: Vec<Diagnostic>,
        kernel: &dyn GeometryKernel,
        started: Instant,
    ) -> EvaluationOutput {
        let part_name = part_name.into();
        let empty = SolidShape::empty(&part_name);
        let chk = kernel.check_solid(&empty);
        let mesh = build_scene_mesh(&empty, Vec::new());
        EvaluationOutput {
            status_ok: false,
            part_name,
            is_assembly: false,
            diagnostics,
            check_report: CheckReport {
                watertight: false,
                volume_mm3: 0.0,
                surface_area_mm2: 0.0,
                bbox_mm: BBox3::default(),
                face_count: 0,
                edge_count: 0,
                open_edge_count: 0,
                issues: chk.issues,
            },
            solid: empty,
            component_solids: HashMap::new(),
            scene_mesh: mesh,
            compile_time_ms: started.elapsed().as_secs_f64() * 1000.0,
            kernel_backend: kernel.backend_name().to_string(),
        }
    }

    fn evaluate_non_empty(
        source: &str,
        workspace: &DesktopWorkspace,
        requested: Option<&str>,
    ) -> Result<UnifiedEvalResult, String> {
        let mode = EvalEngineMode::from_env()?;
        evaluate_non_empty_with_mode(source, workspace, requested, mode)
    }

    fn evaluate_non_empty_with_mode(
        source: &str,
        workspace: &DesktopWorkspace,
        requested: Option<&str>,
        mode: EvalEngineMode,
    ) -> Result<UnifiedEvalResult, String> {
        if source.trim().is_empty() || source.len() > 1024 * 1024 {
            return Err("KCL source must be non-empty and at most 1 MiB".into());
        }
        let root = workspace.root_for(requested)?;
        let resolver = FsAssetResolver::new(root.to_path_buf()).map_err(|err| err.to_string())?;
        let kernel = workspace.kernel.as_ref();
        let started = Instant::now();
        let readiness = inspect_graph_eval_readiness(source);

        if readiness.is_graph_ready()
            && let Some(graph) = readiness.graph.as_ref()
        {
            let output = evaluate_graph_source_output(
                source,
                readiness.flavor,
                graph,
                &resolver,
                Some(root),
                kernel,
                started,
            );
            tracing::info!(
                component = "desktop",
                operation = "evaluate_unified",
                eval_path = EvalPath::GraphEvaluator.as_str(),
                eval_engine_mode = mode.as_str(),
                source_flavor = readiness.flavor.as_str(),
                kernel_backend = kernel.backend_name(),
                status_ok = output.status_ok,
                duration_ms = output.compile_time_ms,
                "evaluated via unified GraphEvaluator path"
            );
            return Ok(UnifiedEvalResult {
                output,
                eval_path: EvalPath::GraphEvaluator,
                fallback_reasons: Vec::new(),
            });
        }

        let mut diagnostics = Vec::new();
        if let Some(err) = readiness.bridge_error.as_ref() {
            diagnostics.push(bridge_error_diagnostic(err));
        } else {
            diagnostics.push(diagnostic_error(
                "E402_GRAPH_EVAL_UNSUPPORTED",
                format!(
                    "GraphEvaluator cannot evaluate unsupported constructs: {}",
                    readiness.fallback_reasons.join("; ")
                ),
            ));
        }
        let part_name = readiness
            .graph
            .as_ref()
            .map(|g| g.graph_id.clone())
            .unwrap_or_else(|| "unknown".to_owned());
        tracing::warn!(
            component = "desktop",
            operation = "evaluate_unified",
            eval_path = EvalPath::GraphEvaluator.as_str(),
            eval_engine_mode = mode.as_str(),
            source_flavor = readiness.flavor.as_str(),
            fallback_reason_count = readiness.fallback_reasons.len(),
            fallback_reasons = %readiness.fallback_reasons.join("; "),
            "graph evaluation rejected unsupported or invalid source"
        );
        Ok(UnifiedEvalResult {
            output: empty_error_output(part_name, diagnostics, kernel, started),
            eval_path: EvalPath::GraphEvaluator,
            fallback_reasons: readiness.fallback_reasons,
        })
    }

    fn evaluate_require_ok(
        request: &ExportRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<EvaluationOutput, String> {
        let file_name = &request.file_name;
        let eval_out = evaluate_non_empty(
            &request.source,
            workspace,
            request.workspace_root.as_deref(),
        )?
        .output;
        if !eval_out.status_ok {
            let errors: Vec<String> = eval_out
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| format!("[{}] {}", d.code, d.message))
                .collect();
            let detail = if errors.is_empty() {
                "evaluation failed geometry/watertight gate".to_string()
            } else {
                errors.join("; ")
            };
            return Err(format!("Cannot export `{file_name}`: {detail}"));
        }
        Ok(eval_out)
    }

    fn build_diagnostics_json(file_name: &str, unified: &UnifiedEvalResult) -> serde_json::Value {
        let eval_out = &unified.output;
        let status = if eval_out.status_ok { "ok" } else { "error" };
        let kind = if eval_out.is_assembly {
            "assembly"
        } else {
            "part"
        };
        let chk = &eval_out.check_report;
        let mut metrics = serde_json::json!({
            "watertight": chk.watertight,
            "open_edge_count": chk.open_edge_count,
            "volume_mm3": chk.volume_mm3,
            "surface_area_mm2": chk.surface_area_mm2,
            "bbox_mm": {
                "min": chk.bbox_mm.min,
                "max": chk.bbox_mm.max,
            },
            "face_count": chk.face_count,
            "edge_count": chk.edge_count,
            "compile_time_ms": eval_out.compile_time_ms,
            "kernel_backend": eval_out.kernel_backend,
            "kernel_authoritative": is_authoritative_kernel_backend(&eval_out.kernel_backend),
            "eval_path": unified.eval_path.as_str(),
        });
        if let Some(obj) = metrics.as_object_mut() {
            if !is_authoritative_kernel_backend(&eval_out.kernel_backend) {
                let reason = if !unified.fallback_reasons.is_empty() {
                    unified.fallback_reasons.join("; ")
                } else if !cfg!(feature = "occt-ffi") {
                    "此构建未启用 occt-ffi，当前为解析预览内核。".to_string()
                } else {
                    "当前内核不是权威 OCCT B-Rep。".to_string()
                };
                obj.insert("kernel_fallback_reason".into(), serde_json::json!(reason));
            }
            if !unified.fallback_reasons.is_empty() {
                obj.insert(
                    "fallback_reasons".into(),
                    serde_json::json!(unified.fallback_reasons),
                );
            }
        }
        serde_json::json!({
            "status": status,
            "file": file_name,
            "part_name": eval_out.part_name,
            "kind": kind,
            "diagnostics": eval_out.diagnostics,
            "components": eval_out.solid.components,
            "compile_time_ms": eval_out.compile_time_ms,
            "metrics": metrics,
        })
    }

    fn resolve_export_path(workspace_root: &Path, out_path: &str) -> Result<PathBuf, String> {
        let trimmed = out_path.trim();
        if trimmed.is_empty() {
            return Err("Export path must not be empty".to_string());
        }
        if trimmed.contains('\0') {
            return Err("Export path must not contain NUL bytes".to_string());
        }
        cad_runtime::resolve_in_workspace(workspace_root, Path::new(trimmed))
            .map_err(|err| err.to_string())
    }

    fn write_optional_output(
        workspace_root: &Path,
        out_path: &str,
        content: &str,
    ) -> Result<(u64, Option<PathBuf>), String> {
        let trimmed = out_path.trim();
        let bytes = content.len() as u64;
        if trimmed.is_empty() {
            return Ok((bytes, None));
        }
        let path = resolve_export_path(workspace_root, trimmed)?;
        cad_runtime::write_file_atomic(&path, content.as_bytes())
            .map_err(|e| format!("Failed to write export file `{trimmed}`: {e}"))?;
        Ok((bytes, Some(path)))
    }

    // Desktop export commands run inside `invoke`, which has no AbortSignal.
    // The webview cancels a trace id. This gate is the publication boundary
    // for STEP, DXF, drawing SVG, and 3MF. In-process kernel and drawing
    // renders still finish; 3MF also stops its cad-cli child. A cancelled
    // trace does not replace the file.
    // Ownership is the output path, shared across those commands. Discard
    // deletes a path only while that trace is still the writer, so a later
    // export of the same path — including a different format — is left alone.
    struct ExportPublication {
        cancelled: HashSet<String>,
        writers: HashMap<String, String>,
    }

    fn publication_gate() -> &'static Mutex<ExportPublication> {
        static GATE: OnceLock<Mutex<ExportPublication>> = OnceLock::new();
        GATE.get_or_init(|| {
            Mutex::new(ExportPublication {
                cancelled: HashSet::new(),
                writers: HashMap::new(),
            })
        })
    }

    fn lock_publication_gate() -> std::sync::MutexGuard<'static, ExportPublication> {
        publication_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn export_trace_id(trace_id: Option<&str>) -> Option<&str> {
        match trace_id.map(str::trim) {
            Some(id) if !id.is_empty() => Some(id),
            _ => None,
        }
    }

    fn take_publication_cancel(trace_id: Option<&str>) -> bool {
        let Some(id) = export_trace_id(trace_id) else {
            return false;
        };
        lock_publication_gate().cancelled.remove(id)
    }

    // Publication hooks: run once, after export bytes exist and before they are written.
    // Each format has its own slot so a STEP hook cannot fire inside DXF or SVG.
    macro_rules! publication_hook {
        ($slot:ident, $arm:ident, $clear:ident, $fire:ident) => {
            thread_local! {
                static $slot: std::cell::RefCell<Option<Box<dyn Fn()>>> =
                    const { std::cell::RefCell::new(None) };
            }

            #[allow(dead_code)]
            pub(super) fn $arm(callback: impl Fn() + 'static) {
                $slot.with(|slot| *slot.borrow_mut() = Some(Box::new(callback)));
            }

            #[allow(dead_code)]
            pub(super) fn $clear() {
                $slot.with(|slot| *slot.borrow_mut() = None);
            }

            fn $fire() {
                if let Some(callback) = $slot.with(|slot| slot.borrow_mut().take()) {
                    callback();
                }
            }
        };
    }

    publication_hook!(
        STEP_EXPORT_BEFORE_COMMIT,
        arm_step_export_before_commit,
        clear_step_export_before_commit,
        before_step_commit
    );
    publication_hook!(
        DXF_EXPORT_BEFORE_COMMIT,
        arm_dxf_export_before_commit,
        clear_dxf_export_before_commit,
        before_dxf_commit
    );
    publication_hook!(
        DRAWING_SVG_EXPORT_BEFORE_COMMIT,
        arm_drawing_svg_export_before_commit,
        clear_drawing_svg_export_before_commit,
        before_drawing_svg_commit
    );
    publication_hook!(
        THREEMF_EXPORT_BEFORE_COMMIT,
        arm_3mf_export_before_commit,
        clear_3mf_export_before_commit,
        before_3mf_commit
    );

    fn cancel_publication(trace_id: &str) {
        let Some(id) = export_trace_id(Some(trace_id)) else {
            return;
        };
        lock_publication_gate().cancelled.insert(id.to_string());
    }

    fn commit_publication(
        workspace_root: &Path,
        trace_id: Option<&str>,
        out_path: &str,
        content: &str,
    ) -> Result<u64, String> {
        let mut gate = lock_publication_gate();
        if let Some(id) = export_trace_id(trace_id) {
            if gate.cancelled.remove(id) {
                return Err(EXPORT_CLIENT_ABORTED.to_string());
            }
        }
        let (bytes, written_path) = write_optional_output(workspace_root, out_path, content)?;
        if let Some(path) = written_path {
            let key = path.to_string_lossy().into_owned();
            // A write with no trace id must clear the previous owner. Otherwise
            // discarding that older trace would delete this newer file.
            match export_trace_id(trace_id) {
                Some(id) => {
                    gate.writers.insert(key, id.to_string());
                }
                None => {
                    gate.writers.remove(&key);
                }
            }
        }
        Ok(bytes)
    }

    fn commit_publication_bytes(
        workspace_root: &Path,
        trace_id: Option<&str>,
        out_path: &str,
        content: &[u8],
    ) -> Result<u64, String> {
        let mut gate = lock_publication_gate();
        if let Some(id) = export_trace_id(trace_id) {
            if gate.cancelled.remove(id) {
                return Err(EXPORT_CLIENT_ABORTED.to_string());
            }
        }
        let (bytes, written_path) = write_output_bytes(workspace_root, out_path, content)?;
        let key = written_path.to_string_lossy().into_owned();
        match export_trace_id(trace_id) {
            Some(id) => {
                gate.writers.insert(key, id.to_string());
            }
            None => {
                gate.writers.remove(&key);
            }
        }
        Ok(bytes)
    }

    fn write_output_bytes(
        workspace_root: &Path,
        out_path: &str,
        content: &[u8],
    ) -> Result<(u64, PathBuf), String> {
        let trimmed = out_path.trim();
        if trimmed.is_empty() {
            return Err("3MF export requires out_path".to_string());
        }
        let path = resolve_export_path(workspace_root, trimmed)?;
        cad_runtime::write_file_atomic(&path, content)
            .map_err(|err| format!("Failed to write export file `{trimmed}`: {err}"))?;
        Ok((content.len() as u64, path))
    }

    fn discard_publication(
        workspace_root: &Path,
        trace_id: &str,
        out_path: &str,
    ) -> Result<bool, String> {
        let Some(id) = export_trace_id(Some(trace_id)) else {
            return Ok(false);
        };
        let trimmed = out_path.trim();
        if trimmed.is_empty() {
            return Ok(false);
        }
        let resolved = resolve_export_path(workspace_root, trimmed)?;
        let key = resolved.to_string_lossy().into_owned();
        let mut gate = lock_publication_gate();
        gate.cancelled.remove(id);
        if gate.writers.get(&key).map(String::as_str) != Some(id) {
            return Ok(false);
        }
        match fs::remove_file(&resolved) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!("Failed to discard export file `{trimmed}`: {err}"));
            }
        }
        gate.writers.remove(&key);
        Ok(true)
    }

    pub(super) fn cancel_export_step_ap242_in(trace_id: &str) {
        cancel_publication(trace_id);
    }

    pub(super) fn commit_step_export(
        workspace_root: &Path,
        trace_id: Option<&str>,
        out_path: &str,
        content: &str,
    ) -> Result<u64, String> {
        commit_publication(workspace_root, trace_id, out_path, content)
    }

    pub(super) fn discard_export_step_ap242_in(
        workspace: &DesktopWorkspace,
        trace_id: &str,
        out_path: &str,
    ) -> Result<bool, String> {
        discard_publication(workspace.root_for(None)?, trace_id, out_path)
    }

    pub(super) fn cancel_export_dxf_drawing_in(trace_id: &str) {
        cancel_publication(trace_id);
    }

    pub(super) fn commit_dxf_export(
        workspace_root: &Path,
        trace_id: Option<&str>,
        out_path: &str,
        content: &str,
    ) -> Result<u64, String> {
        commit_publication(workspace_root, trace_id, out_path, content)
    }

    pub(super) fn discard_export_dxf_drawing_in(
        workspace: &DesktopWorkspace,
        trace_id: &str,
        out_path: &str,
    ) -> Result<bool, String> {
        discard_publication(workspace.root_for(None)?, trace_id, out_path)
    }

    pub(super) fn cancel_export_drawing_svg_in(trace_id: &str) {
        cancel_publication(trace_id);
    }

    pub(super) fn commit_drawing_svg_export(
        workspace_root: &Path,
        trace_id: Option<&str>,
        out_path: &str,
        content: &str,
    ) -> Result<u64, String> {
        commit_publication(workspace_root, trace_id, out_path, content)
    }

    pub(super) fn discard_export_drawing_svg_in(
        workspace: &DesktopWorkspace,
        trace_id: &str,
        out_path: &str,
    ) -> Result<bool, String> {
        discard_publication(workspace.root_for(None)?, trace_id, out_path)
    }

    #[tauri::command]
    pub fn list_workspace_assets(
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let dir = workspace_asset_directory(&workspace)?;
        let mut assets = Vec::new();
        for entry in fs::read_dir(dir)
            .map_err(|_| "Could not list workspace assets")?
            .flatten()
        {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if normalized_asset_name(&name).as_ref() != Some(&name) {
                continue;
            }
            let Ok(info) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if info.is_file()
                && !info.file_type().is_symlink()
                && (1..=MAX_ASSET_BYTES as u64).contains(&info.len())
            {
                assets.push((name, info.len()));
            }
            if assets.len() == 256 {
                break;
            }
        }
        assets.sort_unstable();
        Ok(assets
            .into_iter()
            .map(|(name, size)| describe_asset(&name, size))
            .collect())
    }

    #[tauri::command]
    pub fn upload_workspace_asset(
        request: tauri::ipc::Request<'_>,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<serde_json::Value, String> {
        use std::{
            io::Write,
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT_UPLOAD: AtomicU64 = AtomicU64::new(0);
        let name = request
            .headers()
            .get("x-asset-name")
            .and_then(|v| v.to_str().ok())
            .and_then(normalized_asset_name)
            .ok_or(
                "Use letters, numbers, underscores or hyphens and a STEP, BREP, IGES, STL, 3MF, OBJ, glTF or GLB extension",
            )?;
        let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
            return Err("Expected raw asset bytes".into());
        };
        if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
            return Err("Asset must be non-empty and at most 32 MiB".into());
        }
        let dir = workspace_asset_directory(&workspace)?;
        let stage = dir.join(format!(
            ".upload-{}-{}",
            std::process::id(),
            NEXT_UPLOAD.fetch_add(1, Ordering::Relaxed)
        ));
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .map_err(|_| "Could not stage the workspace asset")?;
        let write_result = out.write_all(bytes);
        drop(out);
        let result = write_result.and_then(|()| fs::hard_link(&stage, dir.join(&name)));
        let _ = fs::remove_file(&stage);
        match result {
            Ok(()) => Ok(describe_asset(&name, bytes.len() as u64)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err("An asset with this name already exists".into())
            }
            Err(_) => Err("Could not save the workspace asset".into()),
        }
    }

    #[tauri::command]
    pub fn get_engine_status() -> EngineStatus {
        engine_status_from_runtime()
    }


    #[tauri::command]
    pub fn compile_kcl(
        request: CompileKclRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<serde_json::Value, String> {
        compile_kcl_in(request, &workspace)
    }

    pub(super) fn compile_kcl_in(
        request: CompileKclRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<serde_json::Value, String> {
        let mode = EvalEngineMode::from_env()?;
        compile_kcl_with_mode(request, workspace, mode)
    }

    pub(super) fn compile_kcl_with_mode(
        request: CompileKclRequest,
        workspace: &DesktopWorkspace,
        mode: EvalEngineMode,
    ) -> Result<serde_json::Value, String> {
        crate::observe::traced("compile_kcl", request.trace_id.as_deref(), |trace_id| {
            let eval_out = evaluate_non_empty_with_mode(
                &request.source,
                workspace,
                request.workspace_root.as_deref(),
                mode,
            )?;
            let mut value = build_diagnostics_json(&request.file_name, &eval_out);
            if let Some(object) = value.as_object_mut() {
                object.insert("trace_id".into(), trace_id.into());
            }
            Ok(value)
        })
    }

    #[tauri::command]
    pub fn build_cad_artifact(
        request: CompileKclRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<serde_json::Value, String> {
        build_cad_artifact_in(request, &workspace)
    }

    pub(super) fn build_cad_artifact_in(
        request: CompileKclRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<serde_json::Value, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("build_cad_artifact", trace_id.as_deref(), |_| {
            serde_json::to_value(evaluate_artifact(request, workspace)?)
                .map_err(|err| err.to_string())
        })
    }

    #[derive(Serialize)]
    struct BuildArtifact {
        diagnostics: serde_json::Value,
        scene_mesh: SceneMeshPayload,
        elapsed_ms: f64,
    }

    fn evaluate_artifact(
        request: CompileKclRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<BuildArtifact, String> {
        let unified = evaluate_non_empty(
            &request.source,
            workspace,
            request.workspace_root.as_deref(),
        )?;
        let diagnostics = build_diagnostics_json(&request.file_name, &unified);
        Ok(BuildArtifact {
            diagnostics,
            scene_mesh: unified.output.scene_mesh,
            elapsed_ms: unified.output.compile_time_ms,
        })
    }

    #[tauri::command]
    pub async fn build_cad_artifact_binary(
        request: CompileKclRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<tauri::ipc::Response, String> {
        let workspace = workspace.inner().clone();
        let bytes = tauri::async_runtime::spawn_blocking(move || {
            build_cad_artifact_binary_in(request, &workspace)
        })
        .await
        .map_err(|err| err.to_string())??;
        Ok(tauri::ipc::Response::new(bytes))
    }

    pub(super) fn build_cad_artifact_binary_in(
        request: CompileKclRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<Vec<u8>, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("build_cad_artifact_binary", trace_id.as_deref(), |_| {
            encode_artifact(evaluate_artifact(request, workspace)?)
        })
    }

    // CMS1: magic, u32 JSON offset, packed mesh arrays, then UTF-8 JSON metadata.
    // Each array has a u32 element count and padding to the element's alignment.
    // Arrays follow face order (vertices, normals, indices) and then edge order (polyline).
    // All numeric values are little-endian. The JSON keeps the arrays as empty placeholders.
    fn encode_artifact(mut artifact: BuildArtifact) -> Result<Vec<u8>, String> {
        let mut bytes = b"CMS1\0\0\0\0".to_vec();
        for face in &mut artifact.scene_mesh.faces {
            write_f64_array(&mut bytes, std::mem::take(&mut face.vertices))?;
            write_f64_array(&mut bytes, std::mem::take(&mut face.normals))?;
            write_u32(&mut bytes, face.indices.len())?;
            let indices = std::mem::take(&mut face.indices);
            bytes.reserve(indices.len() * size_of::<u32>());
            for index in indices {
                bytes.extend_from_slice(&index.to_le_bytes());
            }
        }
        for edge in &mut artifact.scene_mesh.edges {
            write_f64_array(&mut bytes, std::mem::take(&mut edge.polyline))?;
        }
        let metadata_offset = u32::try_from(bytes.len()).map_err(|_| "Scene mesh is too large")?;
        bytes[4..8].copy_from_slice(&metadata_offset.to_le_bytes());
        serde_json::to_writer(&mut bytes, &artifact).map_err(|err| err.to_string())?;
        Ok(bytes)
    }

    #[derive(Serialize)]
    struct Cms2ArtifactMetadata {
        diagnostics: serde_json::Value,
        elapsed_ms: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
        format_version: &'static str,
        edit_handles: Vec<EditHandle>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hole_offsets: Vec<HoleOffsetHandle>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        components: Vec<cad_kernel_occt::ComponentMeta>,
    }

    #[tauri::command]
    pub async fn build_cad_artifact_cms2(
        request: CompileKclRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<tauri::ipc::Response, String> {
        let workspace = workspace.inner().clone();
        let bytes = tauri::async_runtime::spawn_blocking(move || {
            build_cad_artifact_cms2_in(request, &workspace)
        })
        .await
        .map_err(|err| err.to_string())??;
        Ok(tauri::ipc::Response::new(bytes))
    }

    pub(super) fn build_cad_artifact_cms2_in(
        request: CompileKclRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<Vec<u8>, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("build_cad_artifact_cms2", trace_id.as_deref(), |tid| {
            let unified = evaluate_non_empty(
                &request.source,
                workspace,
                request.workspace_root.as_deref(),
            )?;
            let diagnostics = build_diagnostics_json(&request.file_name, &unified);
            let compact = CompactScene::from_solid_shape(&unified.output.solid);
            let cms2_bytes = compact.encode_cms2();
            let metadata = Cms2ArtifactMetadata {
                diagnostics,
                elapsed_ms: unified.output.compile_time_ms,
                trace_id: Some(tid.to_owned()),
                format_version: "0.1",
                edit_handles: unified.output.scene_mesh.edit_handles,
                hole_offsets: unified.output.scene_mesh.hole_offsets,
                components: unified.output.scene_mesh.components,
            };
            let metadata_json = serde_json::to_vec(&metadata).map_err(|err| err.to_string())?;
            encode_cms2_envelope(&metadata_json, &cms2_bytes)
        })
    }

    fn write_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), String> {
        let value = u32::try_from(value).map_err(|_| "Scene mesh array is too large")?;
        bytes.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn write_f64_array(bytes: &mut Vec<u8>, values: Vec<f64>) -> Result<(), String> {
        write_u32(bytes, values.len())?;
        bytes.resize(bytes.len().next_multiple_of(8), 0);
        bytes.reserve(values.len() * size_of::<f64>());
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }

    #[tauri::command]
    pub fn export_step_ap242(
        request: ExportRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<ExportResult, String> {
        export_step_ap242_in(request, &workspace)
    }

    #[tauri::command]
    pub fn cancel_export_step_ap242(request: ExportControlRequest) {
        cancel_export_step_ap242_in(&request.trace_id);
    }

    #[tauri::command]
    pub fn discard_export_step_ap242(
        request: ExportControlRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<bool, String> {
        discard_export_step_ap242_in(&workspace, &request.trace_id, &request.out_path)
    }

    pub(super) fn export_step_ap242_in(
        request: ExportRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<ExportResult, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("export_step_ap242", trace_id.as_deref(), |trace_id| {
            if take_publication_cancel(request.trace_id.as_deref()) {
                return Err(EXPORT_CLIENT_ABORTED.to_string());
            }
            let workspace_root = workspace.root_for(request.workspace_root.as_deref())?;
            if !request.out_path.trim().is_empty() {
                resolve_export_path(workspace_root, &request.out_path)?;
            }
            let mut eval_out = evaluate_require_ok(&request, workspace)?;
            let authoritative = is_authoritative_kernel_backend(&eval_out.kernel_backend);
            if !authoritative && !request.allow_non_authoritative_preview.unwrap_or(false) {
                return Err(
                    "当前为解析预览内核，不能将结果作为权威 STEP AP242 导出。若仅需非权威预览 STEP，请在界面确认后再导出。"
                        .to_string(),
                );
            }
            if !request.include_pmi.unwrap_or(true) {
                eval_out.solid.pmi.clear();
            }
            let pmi_entity_count = eval_out.solid.pmi.len();
            let step_text = workspace.kernel.export_step(&eval_out.solid)?;
            // The kernel call above is not interruptible. Re-check immediately
            // before the file becomes visible to anything else.
            before_step_commit();
            let bytes_written = commit_step_export(
                workspace_root,
                request.trace_id.as_deref(),
                &request.out_path,
                &step_text,
            )?;
            Ok(ExportResult {
                format: "STEP_AP242",
                out_path: request.out_path,
                bytes_written,
                pmi_entity_count,
                trace_id: Some(trace_id.to_owned()),
                message: if authoritative {
                    format!(
                        "Exported STEP AP242 for {} ({} bytes, {} PMI entities)",
                        eval_out.part_name, bytes_written, pmi_entity_count
                    )
                } else {
                    format!(
                        "已导出非权威预览 STEP（{}，{} 字节）。该文件不是精确 OCCT B-Rep。",
                        eval_out.part_name, bytes_written
                    )
                },
                authoritative: Some(authoritative),
                kernel_backend: Some(eval_out.kernel_backend.clone()),
            })
        })
    }

    #[tauri::command]
    pub fn export_dxf_drawing(
        request: ExportRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<ExportResult, String> {
        export_dxf_drawing_in(request, &workspace)
    }

    #[tauri::command]
    pub fn cancel_export_dxf_drawing(request: ExportControlRequest) {
        cancel_export_dxf_drawing_in(&request.trace_id);
    }

    #[tauri::command]
    pub fn discard_export_dxf_drawing(
        request: ExportControlRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<bool, String> {
        discard_export_dxf_drawing_in(&workspace, &request.trace_id, &request.out_path)
    }

    pub(super) fn export_dxf_drawing_in(
        request: ExportRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<ExportResult, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("export_dxf_drawing", trace_id.as_deref(), |trace_id| {
            if take_publication_cancel(request.trace_id.as_deref()) {
                return Err(EXPORT_CLIENT_ABORTED.to_string());
            }
            let workspace_root = workspace.root_for(request.workspace_root.as_deref())?;
            if !request.out_path.trim().is_empty() {
                resolve_export_path(workspace_root, &request.out_path)?;
            }
            let eval_out = evaluate_require_ok(&request, workspace)?;
            let view_count = request.views.as_ref().map_or(3, Vec::len);
            let dxf_text = render_dxf(&eval_out.solid, &eval_out.check_report)?;
            // Drawing render is not interruptible. Re-check immediately before
            // the file becomes visible to anything else.
            before_dxf_commit();
            let bytes_written = commit_dxf_export(
                workspace_root,
                request.trace_id.as_deref(),
                &request.out_path,
                &dxf_text,
            )?;
            Ok(ExportResult {
                format: "DXF",
                out_path: request.out_path,
                bytes_written,
                pmi_entity_count: eval_out.solid.pmi.len(),
                message: format!(
                    "Exported {} orthographic DXF view(s) for {} ({} bytes)",
                    view_count, eval_out.part_name, bytes_written
                ),
                trace_id: Some(trace_id.to_owned()),
                authoritative: None,
                kernel_backend: None,
            })
        })
    }

    #[tauri::command]
    pub fn export_drawing_svg(
        request: ExportRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<ExportResult, String> {
        export_drawing_svg_in(request, &workspace)
    }

    #[tauri::command]
    pub fn cancel_export_drawing_svg(request: ExportControlRequest) {
        cancel_export_drawing_svg_in(&request.trace_id);
    }

    #[tauri::command]
    pub fn discard_export_drawing_svg(
        request: ExportControlRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<bool, String> {
        discard_export_drawing_svg_in(&workspace, &request.trace_id, &request.out_path)
    }

    pub(super) fn export_drawing_svg_in(
        request: ExportRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<ExportResult, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("export_drawing_svg", trace_id.as_deref(), |trace_id| {
            if take_publication_cancel(request.trace_id.as_deref()) {
                return Err(EXPORT_CLIENT_ABORTED.to_string());
            }
            let workspace_root = workspace.root_for(request.workspace_root.as_deref())?;
            if !request.out_path.trim().is_empty() {
                resolve_export_path(workspace_root, &request.out_path)?;
            }
            let eval_out = evaluate_require_ok(&request, workspace)?;
            let svg_text = try_render_drawing_svg(
                &eval_out.solid,
                &eval_out.check_report,
                DrawingOptions::default(),
            )?;
            // Drawing render is not interruptible. Re-check immediately before
            // the file becomes visible to anything else.
            before_drawing_svg_commit();
            let bytes_written = commit_drawing_svg_export(
                workspace_root,
                request.trace_id.as_deref(),
                &request.out_path,
                &svg_text,
            )?;
            Ok(ExportResult {
                format: "DRAWING_SVG",
                out_path: request.out_path,
                bytes_written,
                pmi_entity_count: eval_out.solid.pmi.len(),
                message: format!(
                    "Exported 2D engineering drawing SVG for {} ({} bytes)",
                    eval_out.part_name, bytes_written
                ),
                trace_id: Some(trace_id.to_owned()),
                authoritative: None,
                kernel_backend: None,
            })
        })
    }

    #[tauri::command]
    pub fn export_3mf(
        request: ExportRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<ExportResult, String> {
        export_3mf_in(request, &workspace)
    }

    #[tauri::command]
    pub fn cancel_export_3mf(request: ExportControlRequest) {
        cancel_export_3mf_in(&request.trace_id);
    }

    #[tauri::command]
    pub fn discard_export_3mf(
        request: ExportControlRequest,
        workspace: State<'_, DesktopWorkspace>,
    ) -> Result<bool, String> {
        discard_export_3mf_in(&workspace, &request.trace_id, &request.out_path)
    }

    pub(super) fn cancel_export_3mf_in(trace_id: &str) {
        cancel_publication(trace_id);
    }

    pub(super) fn discard_export_3mf_in(
        workspace: &DesktopWorkspace,
        trace_id: &str,
        out_path: &str,
    ) -> Result<bool, String> {
        discard_publication(workspace.root_for(None)?, trace_id, out_path)
    }

    pub(super) fn export_3mf_in(
        request: ExportRequest,
        workspace: &DesktopWorkspace,
    ) -> Result<ExportResult, String> {
        let trace_id = request.trace_id.clone();
        crate::observe::traced("export_3mf", trace_id.as_deref(), |trace_id| {
            if take_publication_cancel(request.trace_id.as_deref()) {
                return Err(EXPORT_CLIENT_ABORTED.to_string());
            }
            if request.out_path.trim().is_empty() {
                return Err("3MF export requires out_path".to_string());
            }
            let workspace_root = workspace.root_for(request.workspace_root.as_deref())?;
            resolve_export_path(workspace_root, &request.out_path)?;
            let eval_out = evaluate_require_ok(&request, workspace)?;
            let archive_bytes =
                crate::live_attach::encode_solid_3mf(&eval_out.solid, &eval_out.part_name)
                    .map_err(|err| format!("Failed to encode 3MF archive: {err}"))?;
            // 3MF encoding is in-process and non-interruptible; re-check cancel
            // right before the file becomes visible on disk.
            before_3mf_commit();
            let bytes_written = commit_publication_bytes(
                workspace_root,
                request.trace_id.as_deref(),
                &request.out_path,
                &archive_bytes,
            )?;
            Ok(ExportResult {
                format: "3MF",
                out_path: request.out_path,
                bytes_written,
                pmi_entity_count: 0,
                trace_id: Some(trace_id.to_owned()),
                message: format!(
                    "Exported 3MF for {} ({} bytes)",
                    eval_out.part_name, bytes_written
                ),
                authoritative: None,
                kernel_backend: None,
            })
        })
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    observe::init();
    tauri::Builder::default()
        .setup(|app| {
            // A launcher can explicitly open a project; otherwise use private app storage.
            let root = match std::env::var_os("CAD_STUDIO_WORKSPACE_DIR").filter(|p| !p.is_empty())
            {
                Some(path) => PathBuf::from(path),
                None => app.path().app_data_dir()?.join("workspace"),
            };
            let kernel: Arc<dyn GeometryKernel + Send + Sync> =
                Arc::from(default_brep_kernel() as Box<dyn GeometryKernel + Send + Sync>);
            app.manage(DesktopWorkspace::with_kernel(root, kernel)?);
            let attach = live_attach::LiveAttachState::new();
            attach.attach_app(app.handle().clone());
            app.manage(attach);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_engine_status,
            commands::list_workspace_assets,
            commands::upload_workspace_asset,
            commands::compile_kcl,
            commands::build_cad_artifact,
            commands::build_cad_artifact_binary,
            commands::build_cad_artifact_cms2,
            commands::export_step_ap242,
            commands::cancel_export_step_ap242,
            commands::discard_export_step_ap242,
            commands::export_dxf_drawing,
            commands::cancel_export_dxf_drawing,
            commands::discard_export_dxf_drawing,
            commands::export_drawing_svg,
            commands::cancel_export_drawing_svg,
            commands::discard_export_drawing_svg,
            commands::export_3mf,
            commands::cancel_export_3mf,
            commands::discard_export_3mf,
            ai_bridge::get_ai_config,
            ai_bridge::get_ai_profiles,
            ai_bridge::save_ai_profiles,
            ai_bridge::test_ai_connection,
            ai_bridge::run_ai_agent,
            ai_bridge::cancel_ai_agent,
            project_commands::init_aicad_project,
            project_commands::inspect_aicad_project,
            project_commands::apply_aicad_patch,
            project_commands::emit_aicad_kcl,
            project_commands::branch_aicad_project,
            project_commands::slice_aicad_context,
            project_commands::vacuum_aicad_project,
            project_commands::pack_and_go_aicad_project,
            project_commands::open_aicad_document,
            project_commands::import_aicad_document,
            project_commands::save_aicad_document,
            project_commands::undo_aicad_container,
            session_commands::fetch_tree_slice,
            session_commands::search_assembly_tree,
            session_commands::fetch_hlod_chunk,
            session_commands::fetch_interface_digest,
            live_attach::live_attach_start,
            live_attach::live_attach_stop,
            live_attach::live_attach_update_selection,
            live_attach::live_attach_get_status,
            live_attach::live_attach_apply_ui_commands,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

//! Persistent NDJSON transport for in-process kernel operations.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use cad_cadpack::{VirtualAssemblyIndex, build_roi_interface_digest};
use cad_container::{CacheStatus, ContainerError, KernelFingerprint};
use cad_evaluator::{FsAssetResolver, GraphEvaluator, evaluate_source_with_kernel};
use cad_ir::NodeState;
use cad_kernel_occt::GeometryKernel;
use clap::Args;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::project::{CommandOutput, ProjectFormat};
use crate::{build_cli_json, cadpack_cmd, file_parent};

const CADPACK_CACHE_ENTRIES: usize = 8;
type RpcResult<T> = Result<T, RpcError>;

#[derive(Args)]
pub(super) struct DaemonArgs {
    /// Resolve relative paths from this directory and reject workspace escapes
    #[arg(long, value_name = "DIR")]
    workspace_root: Option<PathBuf>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(-32000, format!("{error:#}"))
    }
}

#[derive(Deserialize)]
struct Request {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Option<Value>,
    /// Optional caller trace id; propagated to runtime logs of this request.
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckParams {
    source: String,
    filename: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectParams {
    project_path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DfmParams {
    project_path: String,
    min_wall_mm: f64,
    min_draft_deg: f64,
    pull_dir: [f64; 3],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeParams {
    cadpack_path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    query: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DigestParams {
    cadpack_path: String,
    roi_min_mm: [f64; 3],
    roi_max_mm: [f64; 3],
}

struct Workspace {
    absolute: PathBuf,
    canonical: PathBuf,
    asset_resolver: FsAssetResolver,
}

impl Workspace {
    fn new(root: &Path) -> anyhow::Result<Self> {
        let absolute = std::path::absolute(root)?;
        let canonical = fs::canonicalize(&absolute)?;
        let asset_resolver = FsAssetResolver::new(canonical.clone())?;
        Ok(Self {
            absolute,
            canonical,
            asset_resolver,
        })
    }

    fn resolve(&self, raw_path: &str, allow_missing: bool) -> RpcResult<PathBuf> {
        let outside = || RpcError::invalid_params("path leaves the workspace root");
        if raw_path.split(['/', '\\']).any(|part| part == "..") {
            return Err(RpcError::invalid_params(
                "parent path components are forbidden",
            ));
        }

        let path = Path::new(raw_path);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.canonical)
                .or_else(|_| path.strip_prefix(&self.absolute))
                .map_err(|_| outside())?
        } else {
            path
        };
        if relative.components().any(|component| match component {
            Component::CurDir => false,
            Component::Normal(part) => part.to_string_lossy().contains(':'),
            _ => true,
        }) {
            return Err(RpcError::invalid_params("invalid workspace path component"));
        }

        let mut resolved = self.canonical.clone();
        let mut components = relative.components();
        while let Some(component) = components.next() {
            let Component::Normal(part) = component else {
                continue;
            };
            let candidate = resolved.join(part);
            match fs::canonicalize(&candidate) {
                Ok(canonical) if canonical.starts_with(&self.canonical) => resolved = canonical,
                Ok(_) => return Err(outside()),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    match fs::symlink_metadata(&candidate) {
                        Ok(_) => return Err(outside()),
                        Err(error) if error.kind() != ErrorKind::NotFound => {
                            return Err(RpcError::internal(error));
                        }
                        Err(_) if allow_missing => return Ok(candidate.join(components.as_path())),
                        Err(_) => return Err(RpcError::internal(error)),
                    }
                }
                Err(error) => return Err(RpcError::internal(error)),
            }
        }
        Ok(resolved)
    }
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    size: u64,
}

impl FileStamp {
    fn read(path: &Path) -> RpcResult<Self> {
        let metadata = fs::metadata(path).map_err(RpcError::internal)?;
        if !metadata.is_file() {
            return Err(RpcError::invalid_params("expected a regular CADPack file"));
        }
        Ok(Self {
            modified: metadata.modified().map_err(RpcError::internal)?,
            size: metadata.len(),
        })
    }
}

struct CachedCadpack {
    path: PathBuf,
    stamp: FileStamp,
    index: VirtualAssemblyIndex,
}

struct DaemonState<'a> {
    kernel: &'a dyn GeometryKernel,
    workspace: Option<Workspace>,
    requests_handled: u64,
    cadpacks: VecDeque<CachedCadpack>,
}

impl<'a> DaemonState<'a> {
    fn new(args: DaemonArgs, kernel: &'a dyn GeometryKernel) -> anyhow::Result<Self> {
        let workspace = args
            .workspace_root
            .as_deref()
            .map(Workspace::new)
            .transpose()?;
        Ok(Self {
            kernel,
            workspace,
            requests_handled: 0,
            cadpacks: VecDeque::new(),
        })
    }

    fn path(&self, raw_path: &str, allow_missing: bool) -> RpcResult<PathBuf> {
        if raw_path.is_empty() || raw_path.contains('\0') {
            return Err(RpcError::invalid_params("expected a nonempty file path"));
        }
        match &self.workspace {
            Some(workspace) => workspace.resolve(raw_path, allow_missing),
            None => Ok(PathBuf::from(raw_path)),
        }
    }

    fn project_error(&self, error: anyhow::Error) -> RpcError {
        if self.workspace.is_some()
            && matches!(
                error.downcast_ref::<ContainerError>(),
                Some(ContainerError::UnsafePath(_))
            )
        {
            return RpcError::invalid_params(format!("{error:#}"));
        }
        RpcError::internal(error)
    }

    fn handle_line(&mut self, line: &[u8]) -> (Value, bool) {
        let request = match serde_json::from_slice::<Value>(line) {
            Ok(value) if value.is_object() => serde_json::from_value::<Request>(value).ok(),
            Ok(_) => None,
            Err(_) => {
                return (
                    error_response(Value::Null, RpcError::new(-32700, "Parse error")),
                    false,
                );
            }
        };
        // This transport pairs every input line with a response. Calls must provide an ID.
        let request = match request {
            Some(request)
                if request.jsonrpc == "2.0"
                    && (request.id.is_string()
                        || request.id.is_number()
                        || request.id.is_null()) =>
            {
                request
            }
            _ => {
                return (
                    error_response(Value::Null, RpcError::new(-32600, "Invalid Request")),
                    false,
                );
            }
        };

        self.requests_handled = self.requests_handled.saturating_add(1);
        let trace_id = cad_observe::resolve_trace_id(request.trace_id.as_deref());
        let span = tracing::info_span!(
            parent: None,
            "rpc",
            trace_id = %trace_id,
            component = "cad-cli.daemon",
            operation = %request.method
        );
        let _entered = span.enter();
        tracing::info!("request start");
        let started = std::time::Instant::now();
        let outcome = self.dispatch(&request.method, request.params);
        let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        match outcome {
            Ok(result) => {
                tracing::info!(duration_ms, "request ok");
                (
                    json!({ "jsonrpc": "2.0", "id": request.id, "result": result }),
                    request.method == "shutdown",
                )
            }
            Err(error) => {
                tracing::error!(
                    duration_ms,
                    code = cad_observe::codes::R011_IPC_FAILED,
                    error_type = "RpcError",
                    native_status = error.code,
                    "request failed: {}",
                    error.message
                );
                (error_response(request.id, error), false)
            }
        }
    }

    fn dispatch(&mut self, method: &str, params: Option<Value>) -> RpcResult<Value> {
        match method {
            "ping" => {
                decode_params::<EmptyParams>(params)?;
                Ok(json!({
                    "status": "ok",
                    "protocol": "aicad-daemon-v1",
                    "requests_handled": self.requests_handled,
                }))
            }
            "check_kcl" => self.check_kcl(decode_params(params)?),
            "eval_project" => self.eval_project(decode_params(params)?),
            "dfm_check" => self.dfm_check(decode_params(params)?),
            "cadpack_tree" => self.cadpack_tree(decode_params(params)?),
            "cadpack_digest" => self.cadpack_digest(decode_params(params)?),
            "shutdown" => {
                decode_params::<EmptyParams>(params)?;
                Ok(json!({ "status": "bye" }))
            }
            _ => Err(RpcError::new(-32601, "Method not found")),
        }
    }

    fn check_kcl(&self, params: CheckParams) -> RpcResult<Value> {
        let filename = params.filename.as_deref().unwrap_or("inline.kcl");
        let filename_path = self.path(filename, true)?;
        let local_resolver;
        let resolver = match &self.workspace {
            Some(workspace) => &workspace.asset_resolver,
            None => {
                local_resolver = FsAssetResolver::new(file_parent(&filename_path).to_owned())
                    .map_err(RpcError::internal)?;
                &local_resolver
            }
        };
        let eval = evaluate_source_with_kernel(&params.source, resolver, self.kernel);
        if self.workspace.is_some()
            && let Some(diagnostic) = eval
                .diagnostics
                .iter()
                .find(|d| d.code == "E502_UNSAFE_ASSET_PATH")
        {
            return Err(RpcError::invalid_params(diagnostic.message.clone()));
        }
        let output = build_cli_json(filename, &eval, None);
        Ok(json!({
            "valid": eval.status_ok,
            "diagnostics": output.diagnostics,
            "metrics": output.metrics,
        }))
    }

    fn eval_project(&self, params: ProjectParams) -> RpcResult<Value> {
        let path = self.path(&params.project_path, false)?;
        let project = ProjectFormat::detect(&path)
            .and_then(|format| format.load(&path))
            .map_err(|error| self.project_error(error))?;
        let graph = &project.graph;
        let mut evaluator = GraphEvaluator::new(self.kernel);
        let ws_root = self.workspace.as_ref().map(|ws| ws.canonical.as_path());
        let report = evaluator
            .evaluate_project_at_path(&path, ws_root)
            .map_err(|error| self.project_error(anyhow::Error::new(error)))?;
        if let Some((id, reason)) =
            report
                .node_reports
                .iter()
                .find_map(|(id, node)| match &node.status {
                    NodeState::FailedWithRollback { reason, .. } => Some((id, reason)),
                    _ => None,
                })
        {
            return Err(RpcError::internal(format!(
                "node `{id}` failed evaluation: {reason}"
            )));
        }

        let kernel = KernelFingerprint::new(self.kernel.backend_name(), env!("CARGO_PKG_VERSION"));
        let cache_status = match project.check_cache_status(&kernel) {
            CacheStatus::FreshExactMatch => "fresh_exact_match",
            CacheStatus::KernelVersionDrift { .. } => "kernel_version_drift",
            CacheStatus::GraphModified => "graph_modified",
            CacheStatus::Missing => "missing",
        };
        Ok(json!({
            "project_name": project.manifest.project_name,
            "node_count": graph.nodes.len(),
            "mesh_count": report.output_solids.len() + report.output_implicit_meshes.len() + report.output_subd_meshes.len(),
            "cache_status": cache_status,
        }))
    }

    fn dfm_check(&self, params: DfmParams) -> RpcResult<Value> {
        if params.min_wall_mm < 0.0 {
            return Err(RpcError::invalid_params("min_wall_mm must be nonnegative"));
        }
        let path = self.path(&params.project_path, false)?;
        let output = cadpack_cmd::dfm_check(
            &path,
            params.min_wall_mm,
            params.min_draft_deg,
            params.pull_dir,
            self.kernel,
        )
        .map_err(|error| self.project_error(error))?;
        report_json(output)
    }

    fn cadpack_tree(&mut self, params: TreeParams) -> RpcResult<Value> {
        let index = self.cadpack_index(&params.cadpack_path)?;
        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(50);
        if let Some(query) = params.query {
            let instance_ids = index.search_by_name(&query, offset.saturating_add(limit));
            let page = &instance_ids[offset.min(instance_ids.len())..];
            return Ok(json!({ "query": query, "offset": offset, "instance_ids": page }));
        }
        serde_json::to_value(index.page_slice(offset, limit)).map_err(RpcError::internal)
    }

    fn cadpack_digest(&mut self, params: DigestParams) -> RpcResult<Value> {
        let min = params.roi_min_mm;
        let max = params.roi_max_mm;
        if min.into_iter().zip(max).any(|(min, max)| min > max) {
            return Err(RpcError::invalid_params(
                "ROI minimum must not exceed its maximum",
            ));
        }
        let index = self.cadpack_index(&params.cadpack_path)?;
        serde_json::to_value(build_roi_interface_digest(index, min, max))
            .map_err(RpcError::internal)
    }

    fn cadpack_index(&mut self, raw_path: &str) -> RpcResult<&VirtualAssemblyIndex> {
        let path = self.path(raw_path, false)?;
        let path = if self.workspace.is_none() {
            fs::canonicalize(path).map_err(RpcError::internal)?
        } else {
            path
        };
        let stamp = FileStamp::read(&path)?;
        let cached = self
            .cadpacks
            .iter()
            .position(|entry| entry.path == path)
            .and_then(|position| self.cadpacks.remove(position));
        let entry = match cached {
            Some(entry) if entry.stamp == stamp => entry,
            _ => {
                // Reuse the CLI decoder and its validation for both supported tree chunk layouts.
                let mut page = report_json(
                    cadpack_cmd::tree(&path, 0, usize::MAX, None).map_err(RpcError::internal)?,
                )?;
                let nodes = page
                    .get_mut("nodes")
                    .map(Value::take)
                    .ok_or_else(|| RpcError::internal("CADPack tree report has no nodes"))?;
                let nodes = serde_json::from_value(nodes).map_err(RpcError::internal)?;
                if FileStamp::read(&path)? != stamp {
                    return Err(RpcError::internal(
                        "CADPack changed while its tree was loading",
                    ));
                }
                CachedCadpack {
                    path,
                    stamp,
                    index: VirtualAssemblyIndex::new(nodes),
                }
            }
        };
        self.cadpacks.push_front(entry);
        self.cadpacks.truncate(CADPACK_CACHE_ENTRIES);
        Ok(&self.cadpacks[0].index)
    }
}

fn decode_params<T: DeserializeOwned>(params: Option<Value>) -> RpcResult<T> {
    let params = params.unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Err(RpcError::invalid_params("expected named parameters"));
    }
    serde_json::from_value(params).map_err(|error| RpcError::invalid_params(error.to_string()))
}

fn report_json(output: CommandOutput) -> RpcResult<Value> {
    match output {
        CommandOutput::Report { json, .. } => Ok(json),
        CommandOutput::Kcl(_) => Err(RpcError::internal("expected a JSON report")),
    }
}

fn error_response(id: Value, error: RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn serve(
    state: &mut DaemonState<'_>,
    mut input: impl BufRead,
    mut output: impl Write,
) -> io::Result<()> {
    let mut line = Vec::new();
    while input.read_until(b'\n', &mut line)? != 0 {
        let (response, shutdown) = state.handle_line(&line);
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
        if shutdown {
            break;
        }
        line.clear();
    }
    Ok(())
}

pub(super) fn run(args: DaemonArgs, kernel: &dyn GeometryKernel) -> ExitCode {
    let result = DaemonState::new(args, kernel).and_then(|mut state| {
        serve(&mut state, io::stdin().lock(), io::stdout().lock())?;
        Ok(())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("daemon: {error:#}");
            ExitCode::FAILURE
        }
    }
}

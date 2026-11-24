mod agent_cmd;
mod api_cmd;
mod cadpack_cmd;
mod compare;
mod daemon;
mod decompile_cmd;
mod inspect;
mod live_proxy;
mod mcp_cmd;
mod project;
mod render_export;
mod runtime_host;
mod xcaf_cmd;

use cad_evaluator::{
    AssetResolver, EvaluationOutput, FsAssetResolver, evaluate_graph_source_output,
};
use cad_hir::{Diagnostic, Severity};
use cad_kcl_bridge::{
    EvalEngineMode, EvalPath, bridge_error_diagnostic, inspect_graph_eval_readiness,
};
use cad_kernel_occt::{
    BBox3, CheckReport, GeometryKernel, PmiRecord, SolidShape, SurfaceType, default_brep_kernel,
};
use cad_lsp::{LspPosition, LspServerState};
use cad_render::{
    DrawingOptions, build_scene_mesh, render_dxf, render_dxf_view, try_render_drawing_svg,
    try_render_views_svg,
};
use cad_syntax::{Annotation, Expr, Span, Stmt, TopItem};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "cad-cli",
    version,
    about = "AI-Native CAD-DSL compiler and geometry gate CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check syntax, physical dimensions, sketch constraints, and B-Rep watertightness
    Check {
        #[command(flatten)]
        source: SourceArgs,
        #[command(flatten)]
        external: ExternalCheckArgs,
        #[command(flatten)]
        engine: EvalEngineArgs,
        #[arg(long)]
        json: bool,
    },
    /// Compile a part or assembly and export manufacturing and viewport artifacts
    Build(Box<BuildArgs>),
    /// Query completions, hover and symbol outline without evaluating geometry
    Lsp {
        #[command(flatten)]
        source: SourceArgs,
        /// Cursor line (one-based)
        #[arg(long, requires = "col", value_parser = clap::value_parser!(u32).range(1..))]
        line: Option<u32>,
        /// Cursor column (one-based)
        #[arg(long, requires = "line", value_parser = clap::value_parser!(u32).range(1..))]
        col: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Summarize the geometry and mating features of an external CAD asset
    Inspect(inspect::InspectArgs),
    /// Compare a compiled solid and a reference STEP by exact volume difference
    Compare(compare::CompareArgs),
    /// Recognize prismatic B-Rep features and decompile into a parametric CanonicalGraph
    Decompile(decompile_cmd::DecompileArgs),
    Project(project::ProjectArgs),
    /// Serve persistent JSON-RPC requests on stdin and stdout
    Daemon(daemon::DaemonArgs),
    /// Serve the MCP gateway, or print its OpenAI tool schema
    Mcp(mcp_cmd::McpArgs),
    /// Read the headless session context
    Session(agent_cmd::SessionArgs),
    /// Apply a batch of semantic commands
    Api(api_cmd::ApiArgs),
    /// Read a document summary or explain one node or parameter
    Query(agent_cmd::QueryArgs),
    /// Run the L0–L4 tri-state verification report
    Verify(agent_cmd::VerifyArgs),
    /// Check this binary, the MCP tool budget, workspace access, and live attach
    DoctorAgent(agent_cmd::DoctorArgs),
}

#[derive(Args)]
struct SourceArgs {
    file: PathBuf,
    /// Resolve assets relative to this directory, instead of the source file's directory
    #[arg(long, value_name = "DIR")]
    workspace_root: Option<PathBuf>,
}

impl SourceArgs {
    fn resolver(&self) -> Result<FsAssetResolver, String> {
        let root = self
            .workspace_root
            .as_deref()
            .unwrap_or_else(|| file_parent(&self.file));
        FsAssetResolver::new(root.to_path_buf())
            .map_err(|e| format!("invalid workspace root `{}`: {e}", root.display()))
    }
}

#[derive(Args, Default)]
struct EvalEngineArgs {
    /// Evaluation engine policy: `auto` or `graph` (both execute via GraphEvaluator;
    /// also configurable via CAD_EVAL_ENGINE)
    #[arg(long, value_name = "MODE")]
    eval_engine: Option<String>,
}

impl EvalEngineArgs {
    fn resolve(&self) -> Result<EvalEngineMode, String> {
        EvalEngineMode::resolve(self.eval_engine.as_deref())
    }
}

#[derive(Args)]
struct ExternalCheckArgs {
    /// Require no exact volume overlap with this workspace-relative asset
    #[arg(long, requires = "workspace_root")]
    against_asset: Option<String>,
    /// Verify the reference asset still matches this SHA-256
    #[arg(long, requires = "against_asset")]
    against_sha256: Option<String>,
}

fn file_parent(file: &Path) -> &Path {
    file.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[derive(Args)]
struct BuildArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[command(flatten)]
    engine: EvalEngineArgs,
    /// Write STEP AP242
    #[arg(long)]
    out_step: Option<PathBuf>,
    /// Write scene-mesh JSON
    #[arg(long)]
    out_mesh: Option<PathBuf>,
    /// Write 3MF triangle mesh
    #[arg(long)]
    out_3mf: Option<PathBuf>,
    /// Write front, top, right, and isometric SVGs (exact HLR for native B-Reps)
    #[arg(long)]
    out_views: Option<PathBuf>,
    /// Write 1:1 millimetre DXF of the XY cut projection
    #[arg(long)]
    out_dxf: Option<PathBuf>,
    /// Write the native XY engineering view as DXF with solid and dashed lines (not for cutting)
    #[arg(long)]
    out_dxf_view: Option<PathBuf>,
    /// Write the engineering drawing SVG (exact HLR for native B-Reps)
    #[arg(long)]
    out_drawing: Option<PathBuf>,
    /// Write one shaded PNG, or a directory of view PNGs
    #[arg(long)]
    out_render: Option<PathBuf>,
    /// Comma-separated views: iso, front, top, right
    #[arg(long, requires = "out_render")]
    render_views: Option<String>,
    /// Image size as WIDTHxHEIGHT pixels
    #[arg(long, requires = "out_render")]
    render_size: Option<String>,
    /// Also write a color mask and a JSON legend of face ids
    #[arg(long, requires = "out_render")]
    emit_semantic_mask: bool,
    /// Export a single assembly component, in its local coordinates
    #[arg(long)]
    component: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct BBoxJson {
    min: [f64; 3],
    max: [f64; 3],
}

#[derive(Serialize)]
struct MetricsJson {
    watertight: bool,
    volume_mm3: f64,
    surface_area_mm2: f64,
    bbox_mm: BBoxJson,
    face_count: usize,
    edge_count: usize,
    open_edge_count: usize,
    compile_time_ms: f64,
    kernel_backend: String,
    eval_path: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fallback_reasons: Vec<String>,
}

#[derive(Serialize, Default)]
struct ArtifactsJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    step_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mesh_path: Option<String>,
    #[serde(rename = "3mf_path", skip_serializing_if = "Option::is_none")]
    threemf_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    views_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dxf_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dxf_view_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    drawing_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    render_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_path: Option<String>,
}

#[derive(Serialize)]
struct ExportArtifactJson {
    kind: &'static str,
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct CliJsonOutput {
    status: &'static str,
    file: String,
    part_name: String,
    kind: &'static str,
    diagnostics: Vec<Diagnostic>,
    metrics: MetricsJson,
    pmi_records: Vec<PmiRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    components: Vec<cad_kernel_occt::ComponentMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<ArtifactsJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    exports: Vec<ExportArtifactJson>,
    /// Correlates this output with `cad-cli.jsonl` runtime log events.
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Check { .. } => "check",
        Commands::Build(_) => "build",
        Commands::Lsp { .. } => "lsp",
        Commands::Inspect(_) => "inspect",
        Commands::Compare(_) => "compare",
        Commands::Decompile(_) => "decompile",
        Commands::Project(_) => "project",
        Commands::Daemon(_) => "daemon",
        Commands::Mcp(_) => "mcp",
        Commands::Session(_) => "session",
        Commands::Api(_) => "api",
        Commands::Query(_) => "query",
        Commands::Verify(_) => "verify",
        Commands::DoctorAgent(_) => "doctor-agent",
    }
}

fn main() -> ExitCode {
    // Runtime logs go to `cad-cli.jsonl` only; stdout stays the diagnostics contract.
    if cad_observe::sink::init(cad_observe::sink::SinkConfig::new("cad-cli", "info")).is_err() {
        cad_observe::ids::init_process_trace_id_from_env();
    }
    let cli = Cli::parse();
    let operation = command_name(&cli.command);
    let span = tracing::info_span!("cli", component = "cad-cli", operation = operation);
    let _entered = span.enter();
    tracing::info!("command start");
    let started = std::time::Instant::now();
    let code = run_cli(cli);
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    if code == ExitCode::SUCCESS {
        tracing::info!(duration_ms, "command finished");
    } else {
        tracing::warn!(duration_ms, "command finished with non-zero exit code");
    }
    code
}

fn run_cli(cli: Cli) -> ExitCode {
    // `default_brep_kernel()` selects the authoritative OCCT kernel when built
    // with `--features occt-ffi` and the analytic kernel otherwise.
    let kernel = default_brep_kernel();
    let kernel: &dyn GeometryKernel = &*kernel;
    match cli.command {
        Commands::Check {
            source,
            external,
            engine,
            json,
        } => run_check(source, external, engine, json, kernel),
        Commands::Build(args) => run_build(*args, kernel),
        Commands::Lsp {
            source,
            line,
            col,
            json,
        } => run_lsp(source, line.zip(col), json),
        Commands::Inspect(args) => inspect::run(args, kernel),
        Commands::Compare(args) => compare::run(args, kernel),
        Commands::Decompile(args) => decompile_cmd::run(args, kernel),
        Commands::Project(args) => project::run(args, kernel),
        Commands::Daemon(args) => daemon::run(args, kernel),
        Commands::Mcp(args) => mcp_cmd::run(args),
        Commands::Session(args) => agent_cmd::run_session(args),
        Commands::Api(args) => api_cmd::run(args),
        Commands::Query(args) => agent_cmd::run_query(args),
        Commands::Verify(args) => agent_cmd::run_verify(args),
        Commands::DoctorAgent(args) => agent_cmd::run_doctor(args),
    }
}

fn read_source(source: &SourceArgs) -> Result<(String, FsAssetResolver), String> {
    let src = fs::read_to_string(&source.file)
        .map_err(|e| format!("cannot read file `{}`: {e}", source.file.display()))?;
    Ok((src, source.resolver()?))
}

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
        component_solids: std::collections::HashMap::new(),
        scene_mesh: mesh,
        compile_time_ms: started.elapsed().as_secs_f64() * 1000.0,
        kernel_backend: kernel.backend_name().to_string(),
    }
}

/// Unified KCL/DSL evaluation entrypoint:
/// - Evaluates exclusively via `CanonicalGraph` -> `GraphEvaluator`.
/// - Surfaces explicit structured diagnostics when a source cannot be lowered
///   or contains constructs unsupported by `GraphEvaluator`.
fn evaluate_source_unified(
    source: &str,
    resolver: &dyn AssetResolver,
    workspace_root: Option<&Path>,
    kernel: &dyn GeometryKernel,
    mode: EvalEngineMode,
) -> UnifiedEvalResult {
    let started = Instant::now();
    let readiness = inspect_graph_eval_readiness(source);
    if readiness.is_graph_ready()
        && let Some(graph) = readiness.graph.as_ref()
    {
        let output = evaluate_graph_source_output(
            source,
            readiness.flavor,
            graph,
            resolver,
            workspace_root,
            kernel,
            started,
        );
        tracing::info!(
            component = "cad-cli",
            operation = "evaluate_unified",
            eval_path = EvalPath::GraphEvaluator.as_str(),
            eval_engine_mode = mode.as_str(),
            source_flavor = readiness.flavor.as_str(),
            kernel_backend = kernel.backend_name(),
            status_ok = output.status_ok,
            duration_ms = output.compile_time_ms,
            "evaluated via unified GraphEvaluator path"
        );
        return UnifiedEvalResult {
            output,
            eval_path: EvalPath::GraphEvaluator,
            fallback_reasons: Vec::new(),
        };
    }

    let mut diagnostics = Vec::new();
    if let Some(err) = readiness.bridge_error.as_ref() {
        diagnostics.push(bridge_error_diagnostic(err));
    } else {
        diagnostics.push(Diagnostic {
            code: "E402_GRAPH_EVAL_UNSUPPORTED".to_owned(),
            severity: Severity::Error,
            message: format!(
                "GraphEvaluator cannot evaluate unsupported constructs: {}",
                readiness.fallback_reasons.join("; ")
            ),
            span: Span::default(),
        });
    }
    let part_name = readiness
        .graph
        .as_ref()
        .map(|g| g.graph_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    tracing::warn!(
        component = "cad-cli",
        operation = "evaluate_unified",
        eval_path = EvalPath::GraphEvaluator.as_str(),
        eval_engine_mode = mode.as_str(),
        source_flavor = readiness.flavor.as_str(),
        fallback_reason_count = readiness.fallback_reasons.len(),
        fallback_reasons = %readiness.fallback_reasons.join("; "),
        "graph evaluation rejected unsupported or invalid source"
    );
    UnifiedEvalResult {
        output: empty_error_output(part_name, diagnostics, kernel, started),
        eval_path: EvalPath::GraphEvaluator,
        fallback_reasons: readiness.fallback_reasons,
    }
}

fn run_check(
    source: SourceArgs,
    external: ExternalCheckArgs,
    engine: EvalEngineArgs,
    json: bool,
    kernel: &dyn GeometryKernel,
) -> ExitCode {
    let file_str = source.file.to_string_lossy();
    let mode = match engine.resolve() {
        Ok(mode) => mode,
        Err(e) => return emit_io_error(&file_str, &e, json),
    };
    let (src, resolver) = match read_source(&source) {
        Ok(input) => input,
        Err(e) => return emit_io_error(&file_str, &e, json),
    };
    let ws_root = source
        .workspace_root
        .as_deref()
        .unwrap_or_else(|| file_parent(&source.file));

    let mut unified = evaluate_source_unified(&src, &resolver, Some(ws_root), kernel, mode);
    if let Some(asset) = external.against_asset.filter(|_| unified.output.status_ok)
        && let Err(diagnostic) = check_external_asset(
            &unified.output,
            &resolver,
            &asset,
            external.against_sha256,
            kernel,
        )
    {
        unified.output.status_ok = false;
        unified.output.diagnostics.push(diagnostic);
    }
    let mut output = build_cli_json_with_telemetry(
        &file_str,
        &unified.output,
        None,
        unified.eval_path,
        unified.fallback_reasons,
    );
    if json {
        output
            .pmi_records
            .extend(bound_parameter_pmi(&src, &unified.output.solid));
    }
    log_diagnostic_failure("check", &file_str, &unified.output);
    print_output(&output, json);
    exit_code(unified.output.status_ok)
}

fn check_external_asset(
    eval: &EvaluationOutput,
    resolver: &FsAssetResolver,
    path: &str,
    sha256: Option<String>,
    kernel: &dyn GeometryKernel,
) -> Result<(), Diagnostic> {
    use cad_evaluator::{ImportOptions, evaluate_import_with_kernel};
    use cad_kernel_occt::{InterferenceViolation, KernelError};

    let span = Span::default();
    let options = ImportOptions {
        sha256,
        ..ImportOptions::default()
    };
    let reference = evaluate_import_with_kernel(resolver, path, &options, span, kernel)?;
    let diagnostic = |code: &str, message: String| Diagnostic {
        severity: Severity::Error,
        code: code.into(),
        message,
        span,
    };
    let kernel_error = |err: KernelError| {
        let code = match err {
            KernelError::BackendUnavailable => "E504_UNSUPPORTED_IMPORT_FORMAT",
            KernelError::OpenShell => "E505_IMPORT_NON_MANIFOLD_OR_OPEN",
            KernelError::UnsupportedMixedBoolean => "E507_UNSUPPORTED_MIXED_BOOLEAN",
            _ => "E207_INVALID_FEATURE",
        };
        diagnostic(
            code,
            format!("Cannot check exact external interference: {err}"),
        )
    };
    let check_solid = |shape: &SolidShape| match kernel
        .check_interference(shape, &reference.solid, 0.0)
        .map_err(kernel_error)?
    {
        None => Ok(()),
        Some(InterferenceViolation::Overlap { volume_mm3 }) => Err(diagnostic(
            "E506_INTERFERENCE_DETECTED",
            format!(
                "{} overlaps the external asset by {volume_mm3:.6} mm³",
                shape.part_name
            ),
        )),
        Some(InterferenceViolation::Clearance { distance_mm }) => Err(diagnostic(
            "E506_INTERFERENCE_DETECTED",
            format!("External asset clearance is {distance_mm:.6} mm"),
        )),
    };
    if eval.is_assembly {
        for component in &eval.solid.components {
            let world = eval
                .placed_component_with_kernel(component, kernel)
                .map_err(kernel_error)?;
            check_solid(&world)?;
        }
        Ok(())
    } else {
        check_solid(&eval.solid)
    }
}

fn run_build(args: BuildArgs, kernel: &dyn GeometryKernel) -> ExitCode {
    let file_str = args.source.file.to_string_lossy();
    let mode = match args.engine.resolve() {
        Ok(mode) => mode,
        Err(e) => return emit_io_error(&file_str, &e, args.json),
    };
    let (src, resolver) = match read_source(&args.source) {
        Ok(input) => input,
        Err(e) => return emit_io_error(&file_str, &e, args.json),
    };
    let ws_root = args
        .source
        .workspace_root
        .as_deref()
        .unwrap_or_else(|| file_parent(&args.source.file));
    let unified = evaluate_source_unified(&src, &resolver, Some(ws_root), kernel, mode);
    let eval_path = unified.eval_path;
    let fallback_reasons = unified.fallback_reasons;
    let mut eval = unified.output;
    if let Some(id) = args.component.as_deref().filter(|_| eval.status_ok) {
        select_component(&mut eval, kernel, id);
    }

    let mut artifacts = ArtifactsJson::default();
    let mut exports = Vec::new();
    let eval_ok = eval.status_ok;
    // A rejected fillet or chamfer rolls back to the previous solid. The preview
    // mesh still describes that solid, so write it even when the build is not ok.
    // STEP and the other exchange files stay gated on the compilation result so a
    // failed mesh write cannot suppress independent exchange exports.
    if let Some(path) = args.out_mesh.as_ref()
        && (eval_ok || !eval.scene_mesh.faces.is_empty())
    {
        let content = serde_json::to_string_pretty(&eval.scene_mesh).map_err(|e| e.to_string());
        artifacts.mesh_path = export_text_artifact(
            &mut eval,
            &mut exports,
            "mesh",
            path,
            "scene-mesh JSON",
            content,
        );
    }
    if eval_ok {
        if let Some(path) = args.out_step {
            // Opt-in pause so a headless test can abort this process before STEP
            // export returns. Unset in production, so normal exports are not delayed.
            hold_real_step_export_for_test(&path, kernel.backend_name());
            let content = kernel.export_step(&eval.solid);
            artifacts.step_path =
                export_text_artifact(&mut eval, &mut exports, "step", &path, "STEP", content);
        }
        if let Some(path) = args.out_3mf {
            artifacts.threemf_path = export_3mf_artifact(&mut eval, &mut exports, &path);
        }
        if let Some(dir) = args.out_views {
            let dir_str = dir.to_string_lossy().into_owned();
            match try_render_views_svg(&eval.solid, &eval.check_report) {
                Ok(views) => {
                    let mut ok = true;
                    let mut total_bytes = 0_u64;
                    let mut first_error = None;
                    for (name, content) in [
                        ("front.svg", views.front_svg),
                        ("top.svg", views.top_svg),
                        ("right.svg", views.right_svg),
                        ("iso.svg", views.iso_svg),
                    ] {
                        let view_path = dir.join(name);
                        match write_text_file(&view_path, &content) {
                            Ok(bytes) => total_bytes += bytes,
                            Err(e) => {
                                let message = format!(
                                    "failed to export SVG view to `{}`: {e}",
                                    view_path.display()
                                );
                                export_error(&mut eval, message.clone());
                                first_error.get_or_insert(message);
                                ok = false;
                            }
                        }
                    }
                    if ok {
                        artifacts.views_dir = Some(dir_str.clone());
                        exports.push(ExportArtifactJson {
                            kind: "views",
                            path: dir_str,
                            status: "ok",
                            bytes: Some(total_bytes),
                            error: None,
                        });
                    } else {
                        exports.push(ExportArtifactJson {
                            kind: "views",
                            path: dir_str,
                            status: "error",
                            bytes: None,
                            error: first_error,
                        });
                    }
                }
                Err(e) => {
                    let message = format!("SVG view export: {e}");
                    export_error(&mut eval, message.clone());
                    exports.push(ExportArtifactJson {
                        kind: "views",
                        path: dir_str,
                        status: "error",
                        bytes: None,
                        error: Some(message),
                    });
                }
            }
        }
        if let Some(path) = args.out_dxf {
            let content = render_dxf(&eval.solid, &eval.check_report);
            artifacts.dxf_path =
                export_text_artifact(&mut eval, &mut exports, "dxf", &path, "DXF", content);
        }
        if let Some(path) = args.out_dxf_view {
            let content = render_dxf_view(&eval.solid, &eval.check_report);
            artifacts.dxf_view_path = export_text_artifact(
                &mut eval,
                &mut exports,
                "dxf_view",
                &path,
                "DXF engineering view",
                content,
            );
        }
        if let Some(path) = args.out_drawing {
            let parsed = cad_syntax::parse(&src);
            let ann = &parsed.file.annotations;
            let attribute = |path, name| annotation_string(ann, path, name);
            let pmi = drawing_parameter_pmi(&parsed.file.items, &eval);
            let content = try_render_drawing_svg(
                &eval.solid,
                &eval.check_report,
                DrawingOptions {
                    material: attribute("material", None),
                    manufacturing: attribute("manufacturing", Some("method")),
                    additional_pmi: &pmi,
                },
            );
            artifacts.drawing_path = export_text_artifact(
                &mut eval,
                &mut exports,
                "drawing",
                &path,
                "engineering drawing",
                content,
            );
        }
        if let Some(path) = &args.out_render {
            let views = args
                .render_views
                .as_deref()
                .unwrap_or("iso,front,top,right");
            let size = args.render_size.as_deref().unwrap_or("1024x768");
            match render_export::export(
                &eval.solid,
                &eval.check_report.bbox_mm,
                path,
                views,
                size,
                args.emit_semantic_mask,
            ) {
                Ok(paths) => {
                    let render_bytes = path_byte_size(Path::new(&paths.output));
                    exports.push(ExportArtifactJson {
                        kind: "render",
                        path: paths.output.clone(),
                        status: "ok",
                        bytes: render_bytes,
                        error: None,
                    });
                    if let Some(semantic) = &paths.semantic {
                        exports.push(ExportArtifactJson {
                            kind: "semantic",
                            path: semantic.clone(),
                            status: "ok",
                            bytes: path_byte_size(Path::new(semantic)),
                            error: None,
                        });
                    }
                    artifacts.render_path = Some(paths.output);
                    artifacts.semantic_path = paths.semantic;
                }
                Err(error) => {
                    let message = format!("render export: {error}");
                    export_error(&mut eval, message.clone());
                    exports.push(ExportArtifactJson {
                        kind: "render",
                        path: path.to_string_lossy().into_owned(),
                        status: "error",
                        bytes: None,
                        error: Some(message),
                    });
                }
            }
        }
    }

    let mut output = build_cli_json_with_telemetry(
        &file_str,
        &eval,
        Some(artifacts),
        eval_path,
        fallback_reasons,
    );
    output.exports = exports;
    if args.json {
        output
            .pmi_records
            .extend(bound_parameter_pmi(&src, &eval.solid));
    }
    log_diagnostic_failure("build", &file_str, &eval);
    print_output(&output, args.json);
    exit_code(eval.status_ok)
}

fn select_component(eval: &mut EvaluationOutput, kernel: &dyn GeometryKernel, id: &str) {
    if !eval.is_assembly {
        export_error(eval, "--component requires an assembly target".into());
    } else if let Some(solid) = eval.component_solids.remove(id) {
        eval.part_name = solid.part_name.clone();
        eval.check_report = kernel.check_solid(&solid);
        eval.scene_mesh = build_scene_mesh(&solid, Vec::new());
        eval.solid = solid;
        eval.is_assembly = false;
        if !eval.check_report.watertight {
            export_error(
                eval,
                format!(
                    "component `{id}` is not watertight: {:?}",
                    eval.check_report.issues
                ),
            );
        }
    } else {
        let available: Vec<_> = eval
            .solid
            .components
            .iter()
            .map(|c| c.component_id.as_str())
            .collect();
        export_error(
            eval,
            format!(
                "unknown component `{id}`; available IDs: {}",
                available.join(", ")
            ),
        );
    }
}

/// Directory named by `CAD_EXPORT_TEST_HOLD`. When that variable is unset or
/// empty this returns immediately.
///
/// The studio OCCT cancel test sets it so `cad-cli build --out-step` stays
/// alive after the model is evaluated and before `export_step` runs. The
/// process writes a partial STEP marker, then `pid`, `kernel`, `phase`, and
/// `step-path` under the directory, then `ready`. It polls `release` until
/// that file appears or `CAD_EXPORT_TEST_HOLD_MS` elapses (default 30s,
/// clamped to 1ms..=120s). Killing the process in that window is an in-flight
/// cancel. A later build without the variable performs the normal OCCT export.
fn hold_real_step_export_for_test(step_path: &Path, kernel_backend: &str) {
    let Some(dir) = std::env::var_os("CAD_EXPORT_TEST_HOLD") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    let dir = PathBuf::from(dir);
    if let Some(parent) = step_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let _ = fs::create_dir_all(parent);
    }
    // Replaced by the real STEP writer if this build is released instead of killed.
    let _ = fs::write(step_path, "ISO-10303-21;\nPARTIAL_OCCT_EXPORT\n");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path_text = step_path.to_string_lossy();
    let _ = fs::write(dir.join("step-path"), path_text.as_ref());
    let _ = fs::write(dir.join("kernel"), kernel_backend);
    let _ = fs::write(dir.join("phase"), "before-step-export");
    let _ = fs::write(dir.join("pid"), std::process::id().to_string());
    let _ = fs::write(dir.join("ready"), "1");

    let hold_ms = std::env::var("CAD_EXPORT_TEST_HOLD_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(1, 120_000);
    let deadline = Instant::now() + Duration::from_millis(hold_ms);
    let release = dir.join("release");
    while Instant::now() < deadline {
        if release.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn write_text_file(path: &Path, content: &str) -> std::io::Result<u64> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(content.len() as u64)
}

fn export_text_artifact(
    eval: &mut EvaluationOutput,
    exports: &mut Vec<ExportArtifactJson>,
    kind: &'static str,
    path: &Path,
    format: &str,
    data: Result<String, String>,
) -> Option<String> {
    let path_str = path.to_string_lossy().into_owned();
    match data.and_then(|content| write_text_file(path, &content).map_err(|e| e.to_string())) {
        Ok(bytes) => {
            exports.push(ExportArtifactJson {
                kind,
                path: path_str.clone(),
                status: "ok",
                bytes: Some(bytes),
                error: None,
            });
            Some(path_str)
        }
        Err(e) => {
            let message = format!("failed to export {format} to `{}`: {e}", path.display());
            export_error(eval, message.clone());
            exports.push(ExportArtifactJson {
                kind,
                path: path_str,
                status: "error",
                bytes: None,
                error: Some(message),
            });
            None
        }
    }
}

fn path_byte_size(path: &Path) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_file() {
        return Some(meta.len());
    }
    if meta.is_dir() {
        let mut total = 0_u64;
        for entry in fs::read_dir(path).ok()?.flatten() {
            if let Ok(entry_meta) = entry.metadata()
                && entry_meta.is_file()
            {
                total += entry_meta.len();
            }
        }
        return Some(total);
    }
    None
}

fn export_error(eval: &mut EvaluationOutput, message: String) {
    eval.status_ok = false;
    eval.diagnostics.push(Diagnostic {
        code: "E401_EXPORT_FAILED".into(),
        severity: Severity::Error,
        message,
        span: Span::default(),
    });
}

fn export_3mf_artifact(
    eval: &mut EvaluationOutput,
    exports: &mut Vec<ExportArtifactJson>,
    path: &Path,
) -> Option<String> {
    let path_str = path.to_string_lossy().into_owned();
    let result = (|| -> Result<u64, String> {
        let mesh = solid_to_mesh(&eval.solid)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let model_name = if eval.part_name.is_empty() {
            "model"
        } else {
            eval.part_name.as_str()
        };
        cad_mesh_io::write_3mf_mesh_file(path, &mesh, model_name).map_err(|e| e.to_string())?;
        path_byte_size(path).ok_or_else(|| "cannot read exported 3MF file size".to_string())
    })();
    match result {
        Ok(bytes) => {
            exports.push(ExportArtifactJson {
                kind: "3mf",
                path: path_str.clone(),
                status: "ok",
                bytes: Some(bytes),
                error: None,
            });
            Some(path_str)
        }
        Err(e) => {
            let message = format!("failed to export 3MF to `{}`: {e}", path.display());
            export_error(eval, message.clone());
            exports.push(ExportArtifactJson {
                kind: "3mf",
                path: path_str,
                status: "error",
                bytes: None,
                error: Some(message),
            });
            None
        }
    }
}

fn solid_to_mesh(solid: &SolidShape) -> Result<cad_mesh_io::Mesh, String> {
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for (face_index, face) in solid.faces.iter().enumerate() {
        let (vert_chunks, vert_rem) = face.vertices.as_chunks::<3>();
        if !vert_rem.is_empty() {
            return Err(format!(
                "B-Rep face {face_index} has incomplete vertex coordinates"
            ));
        }
        let (idx_chunks, idx_rem) = face.indices.as_chunks::<3>();
        if !idx_rem.is_empty() {
            return Err(format!(
                "B-Rep face {face_index} has incomplete triangle indices"
            ));
        }
        let offset = u32::try_from(vertices.len())
            .map_err(|_| "B-Rep mesh has too many vertices".to_string())?;
        let vertex_count = u32::try_from(vert_chunks.len())
            .map_err(|_| "B-Rep mesh has too many vertices".to_string())?;
        vertices.extend_from_slice(vert_chunks);
        for index in idx_chunks {
            if index.iter().any(|&i| i >= vertex_count) {
                return Err(format!(
                    "B-Rep face {face_index} references an invalid vertex"
                ));
            }
            triangles.push([offset + index[0], offset + index[1], offset + index[2]]);
        }
    }
    let mut mesh = cad_mesh_io::Mesh {
        vertices,
        triangles,
    };
    mesh.weld_vertices(1e-6);
    Ok(mesh)
}

fn first_error_diagnostic(eval: &EvaluationOutput) -> Option<&Diagnostic> {
    eval.diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .or_else(|| eval.diagnostics.first())
}

fn log_diagnostic_failure(operation: &'static str, file: &str, eval: &EvaluationOutput) {
    if eval.status_ok {
        return;
    }
    let first_error = first_error_diagnostic(eval);
    let diag_code = first_error.map_or("E_UNKNOWN", |d| d.code.as_str());
    let detail = first_error
        .map(|d| cad_observe::sanitize_message(&d.message))
        .unwrap_or_else(|| "evaluation failed".to_string());
    let is_io = diag_code == "E001_IO_ERROR";
    if is_io {
        tracing::error!(
            component = "cad-cli",
            operation = operation,
            code = cad_observe::codes::R016_IO_ERROR,
            error_type = "IoError",
            path = file,
            "{diag_code}: {detail}"
        );
    } else {
        tracing::error!(
            component = "cad-cli",
            operation = operation,
            error_type = diag_code,
            path = file,
            "{diag_code}: {detail}"
        );
    }
}

fn annotation_string<'a>(
    annotations: &'a [Annotation],
    path: &str,
    name: Option<&str>,
) -> Option<&'a str> {
    annotations
        .iter()
        .filter(|a| a.path.join("::") == path)
        .flat_map(|a| &a.args)
        .find_map(|arg| match &arg.value {
            Expr::Str { value, .. } if arg.name.as_deref() == name => Some(value.as_str()),
            _ => None,
        })
}

fn drawing_parameter_pmi(items: &[TopItem], eval: &EvaluationOutput) -> Vec<PmiRecord> {
    let mut pmi = Vec::new();
    for item in items {
        let (name, params) = match item {
            TopItem::Part(p) => (&p.name, &p.params),
            TopItem::Assembly(a) => (&a.name, &a.params),
            TopItem::Const(_) => continue,
        };
        if *name != eval.part_name && !eval.solid.components.iter().any(|c| c.part_name == *name) {
            continue;
        }
        for param in params {
            let anno = &param.annotations;
            if let Some(label) = annotation_string(anno, "pmi::tolerance", None) {
                let label = match annotation_string(anno, "pmi::tolerance", Some("roughness")) {
                    Some(roughness) => format!("{label} · {roughness}"),
                    None => label.into(),
                };
                pmi.push(PmiRecord {
                    kind: "pmi::tolerance".into(),
                    label,
                    tolerance_mm: None,
                    datum: None,
                    target_tags: vec![format!("{name}::{}", param.name)],
                    span: param.span,
                });
            }
        }
    }
    pmi
}

fn bound_parameter_pmi(source: &str, solid: &SolidShape) -> Vec<PmiRecord> {
    let parsed = cad_syntax::parse(source);
    let mut records = Vec::new();
    for item in &parsed.file.items {
        let TopItem::Part(part) = item else { continue };
        for param in &part.params {
            let Some(label) = annotation_string(&param.annotations, "pmi::tolerance", None) else {
                continue;
            };
            // A shadowing let still reads the parameter in its own initializer.
            let stmts = part
                .body
                .stmts
                .split_inclusive(
                    |stmt| matches!(stmt, Stmt::Let(binding) if binding.name == param.name),
                )
                .next()
                .unwrap_or_default();
            // A surviving cylindrical face retains the span of the call that set its diameter.
            let mut matching_faces = solid.faces.iter().filter(|face| {
                face.surface_type == SurfaceType::Cylinder
                    && stmts.iter().any(|stmt| {
                        let value = match stmt {
                            Stmt::Let(binding) => &binding.value,
                            Stmt::Return(ret) => &ret.value,
                            Stmt::Expr(expr) => expr,
                        };
                        cylinder_diameter_parameter(value, face.ast_span)
                            == Some(param.name.as_str())
                    })
            });
            if let Some(first_face) = matching_faces.next() {
                let mut target_tags = first_face.provenance_tags.clone();
                target_tags.extend(matching_faces.flat_map(|face| face.provenance_tags.clone()));
                target_tags.sort_unstable();
                target_tags.dedup();
                records.push(PmiRecord {
                    kind: "pmi::tolerance".into(),
                    label: label.into(),
                    tolerance_mm: None,
                    datum: None,
                    target_tags,
                    span: param.span,
                });
            }
        }
    }
    records
}

fn cylinder_diameter_parameter(expr: &Expr, target: Span) -> Option<&str> {
    let span = expr.span();
    if target.start_byte < span.start_byte || target.end_byte > span.end_byte {
        return None;
    }
    match expr {
        Expr::Call { callee, args, .. } => {
            if span == target
                && let Expr::Path { segments, .. } = callee.as_ref()
                && (segments == &["cylinder"] || segments == &["hole", "simple"])
                && let Some(diameter) = args.iter().find(|arg| arg.name.as_deref() == Some("d"))
            {
                let mut value = &diameter.value;
                while let Expr::Paren { inner, .. } = value {
                    value = inner;
                }
                if let Expr::Path { segments, .. } = value
                    && let [name] = segments.as_slice()
                {
                    return Some(name);
                }
            }
            args.iter()
                .find_map(|arg| cylinder_diameter_parameter(&arg.value, target))
        }
        Expr::MethodCall { receiver, args, .. } => cylinder_diameter_parameter(receiver, target)
            .or_else(|| {
                args.iter()
                    .find_map(|arg| cylinder_diameter_parameter(&arg.value, target))
            }),
        Expr::Pipe { left, right, .. } => cylinder_diameter_parameter(left, target)
            .or_else(|| cylinder_diameter_parameter(right, target)),
        Expr::Paren { inner, .. } => cylinder_diameter_parameter(inner, target),
        Expr::Array { elements, .. } => elements
            .iter()
            .find_map(|element| cylinder_diameter_parameter(element, target)),
        _ => None,
    }
}

fn run_lsp(source: SourceArgs, cursor: Option<(u32, u32)>, json: bool) -> ExitCode {
    let file_str = source.file.to_string_lossy();
    let mut server = LspServerState::new();
    // LSP is static: validate the same workspace boundary, without importing geometry.
    let (src, _resolver) = match read_source(&source) {
        Ok(input) => input,
        Err(message) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"status":"error", "file":file_str, "diagnostics":[{
                    "code":"E001_IO_ERROR", "severity":1, "source":"cad-cli", "message":message,
                    "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}}
                }], "variables":[], "symbols":[], "completions":[], "hover":null})
                );
            } else {
            }
            eprintln!("error: {message}");
            tracing::error!(code = cad_observe::codes::R016_IO_ERROR, error_type = "IoError", "{}", message);
            return ExitCode::from(2);
        }
    };
    server.open_or_update(file_str.as_ref(), &src);
    let pos = cursor.map(|(line, col)| LspPosition {
        line: line - 1,
        character: col - 1,
    });
    let result = server.query(&file_str, pos).unwrap();
    let ok = !result.diagnostics.iter().any(|d| d.severity == 1);
    let status = if ok { "ok" } else { "error" };
    if json {
        #[derive(Serialize)]
        struct Output<'a> {
            status: &'static str,
            file: &'a str,
            #[serde(flatten)]
            result: cad_lsp::QueryResult,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Output {
                status,
                file: &file_str,
                result
            })
            .unwrap()
        );
    } else {
        println!("[{status}] {file_str}");
        if let Some(hover) = result.hover {
            println!("{}", hover.summary);
        }
        for symbol in result.symbols {
            println!("  {}", symbol.name);
        }
        for d in result.diagnostics {
            eprintln!("  [{}] {}", d.code, d.message);
        }
    }
    exit_code(ok)
}

fn exit_code(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn build_cli_json(
    file_str: &str,
    eval: &cad_evaluator::EvaluationOutput,
    artifacts: Option<ArtifactsJson>,
) -> CliJsonOutput {
    build_cli_json_with_telemetry(
        file_str,
        eval,
        artifacts,
        EvalPath::GraphEvaluator,
        Vec::new(),
    )
}

fn build_cli_json_with_telemetry(
    file_str: &str,
    eval: &cad_evaluator::EvaluationOutput,
    artifacts: Option<ArtifactsJson>,
    eval_path: EvalPath,
    fallback_reasons: Vec<String>,
) -> CliJsonOutput {
    CliJsonOutput {
        status: if eval.status_ok { "ok" } else { "error" },
        file: file_str.to_string(),
        part_name: eval.part_name.clone(),
        kind: if eval.is_assembly { "assembly" } else { "part" },
        diagnostics: eval.diagnostics.clone(),
        metrics: MetricsJson {
            watertight: eval.check_report.watertight,
            volume_mm3: (eval.check_report.volume_mm3 * 1000.0).round() / 1000.0,
            surface_area_mm2: (eval.check_report.surface_area_mm2 * 1000.0).round() / 1000.0,
            bbox_mm: BBoxJson {
                min: eval.check_report.bbox_mm.min,
                max: eval.check_report.bbox_mm.max,
            },
            face_count: eval.check_report.face_count,
            edge_count: eval.check_report.edge_count,
            open_edge_count: eval.check_report.open_edge_count,
            compile_time_ms: (eval.compile_time_ms * 100.0).round() / 100.0,
            kernel_backend: eval.kernel_backend.clone(),
            eval_path: eval_path.as_str(),
            fallback_reasons,
        },
        pmi_records: eval.solid.pmi.clone(),
        components: eval.solid.components.clone(),
        artifacts,
        exports: Vec::new(),
        trace_id: cad_observe::current_trace_id().map(str::to_owned),
    }
}

fn print_output(output: &CliJsonOutput, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(output).unwrap());
    } else {
        println!(
            "[{}] {} (part={}, watertight={}, volume={:.2} mm3, faces={}, edges={})",
            output.status.to_uppercase(),
            output.file,
            output.part_name,
            output.metrics.watertight,
            output.metrics.volume_mm3,
            output.metrics.face_count,
            output.metrics.edge_count
        );
        for d in &output.diagnostics {
            println!(
                "  {:?} [{}] {}:{}: {}",
                d.severity, d.code, d.span.start_line, d.span.start_col, d.message
            );
        }
    }
}

fn emit_io_error(file_str: &str, msg: &str, json: bool) -> ExitCode {
    let output = CliJsonOutput {
        status: "error",
        file: file_str.to_string(),
        part_name: String::new(),
        kind: "part",
        diagnostics: vec![Diagnostic {
            code: "E001_IO_ERROR".to_string(),
            severity: cad_hir::Severity::Error,
            message: msg.into(),
            span: cad_syntax::Span::default(),
        }],
        metrics: MetricsJson {
            watertight: false,
            volume_mm3: 0.0,
            surface_area_mm2: 0.0,
            bbox_mm: BBoxJson {
                min: [0.0, 0.0, 0.0],
                max: [0.0, 0.0, 0.0],
            },
            face_count: 0,
            edge_count: 0,
            open_edge_count: 0,
            compile_time_ms: 0.0,
            kernel_backend: "rust-brep-mvp".to_string(),
            eval_path: EvalPath::GraphEvaluator.as_str(),
            fallback_reasons: Vec::new(),
        },
        pmi_records: Vec::new(),
        components: Vec::new(),
        artifacts: None,
        exports: Vec::new(),
        trace_id: cad_observe::current_trace_id().map(str::to_owned),
    };
    print_output(&output, json);
    eprintln!("error: {file_str}: {msg}");
    tracing::error!(code = cad_observe::codes::R016_IO_ERROR, error_type = "IoError", "{}", format!("{file_str}: {msg}"));
    ExitCode::from(2)
}

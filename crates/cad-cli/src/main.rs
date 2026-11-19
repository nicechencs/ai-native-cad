use cad_evaluator::{EvaluationOutput, evaluate_source};
use cad_hir::{Diagnostic, Severity};
use cad_kernel_occt::{AnalyticKernel, GeometryKernel, PmiRecord};
use cad_lsp::{LspPosition, LspServerState};
use cad_render::{
    DrawingOptions, build_scene_mesh, render_drawing_svg, render_dxf, render_views_svg,
};
use cad_syntax::{Annotation, Expr, Span, TopItem};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Compile a part or assembly and export manufacturing and viewport artifacts
    Build(BuildArgs),
    /// Query completions, hover and symbol outline without evaluating geometry
    Lsp {
        file: PathBuf,
        /// Cursor line (one-based)
        #[arg(long, requires = "col", value_parser = clap::value_parser!(u32).range(1..))]
        line: Option<u32>,
        /// Cursor column (one-based)
        #[arg(long, requires = "line", value_parser = clap::value_parser!(u32).range(1..))]
        col: Option<u32>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct BuildArgs {
    file: PathBuf,
    /// Write STEP AP242
    #[arg(long)]
    out_step: Option<PathBuf>,
    /// Write scene-mesh JSON
    #[arg(long)]
    out_mesh: Option<PathBuf>,
    /// Write front, top, right, and isometric SVGs to this directory
    #[arg(long)]
    out_views: Option<PathBuf>,
    /// Write 1:1 millimetre DXF of the XY cut projection
    #[arg(long)]
    out_dxf: Option<PathBuf>,
    /// Write the engineering drawing SVG
    #[arg(long)]
    out_drawing: Option<PathBuf>,
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
}

#[derive(Serialize, Default)]
struct ArtifactsJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    step_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mesh_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    views_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dxf_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    drawing_path: Option<String>,
}

#[derive(Serialize)]
struct CliJsonOutput {
    status: &'static str,
    file: String,
    part_name: String,
    kind: &'static str,
    diagnostics: Vec<Diagnostic>,
    metrics: MetricsJson,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    components: Vec<cad_kernel_occt::ComponentMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<ArtifactsJson>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check { file, json } => run_check(&file, json),
        Commands::Build(args) => run_build(args),
        Commands::Lsp {
            file,
            line,
            col,
            json,
        } => run_lsp(&file, line.zip(col), json),
    }
}

fn run_check(file: &PathBuf, json: bool) -> ExitCode {
    let file_str = file.to_string_lossy().to_string();
    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            return emit_io_error(&file_str, &e.to_string(), json);
        }
    };

    let eval = evaluate_source(&src);
    let output = build_cli_json(&file_str, &eval, None);
    print_output(&output, json);
    if eval.status_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_build(args: BuildArgs) -> ExitCode {
    let file_str = args.file.to_string_lossy();
    let src = match fs::read_to_string(&args.file) {
        Ok(s) => s,
        Err(e) => return emit_io_error(&file_str, &e.to_string(), args.json),
    };
    let kernel = AnalyticKernel::new();
    let mut eval = evaluate_source(&src);
    if let Some(id) = args.component.as_deref().filter(|_| eval.status_ok) {
        select_component(&mut eval, &kernel, id);
    }

    let mut artifacts = ArtifactsJson::default();
    if eval.status_ok {
        if let Some(path) = args.out_step {
            let content = kernel.export_step(&eval.solid);
            artifacts.step_path = export_text(&mut eval, &path, "STEP", Ok(content));
        }
        if let Some(path) = args.out_mesh {
            let content = serde_json::to_string_pretty(&eval.scene_mesh).map_err(|e| e.to_string());
            artifacts.mesh_path = export_text(&mut eval, &path, "scene-mesh JSON", content);
        }
        if let Some(dir) = args.out_views {
            let views = render_views_svg(&eval.solid, &eval.check_report);
            let mut ok = true;
            for (name, content) in [
                ("front.svg", views.front_svg),
                ("top.svg", views.top_svg),
                ("right.svg", views.right_svg),
                ("iso.svg", views.iso_svg),
            ] {
                ok &= export_text(&mut eval, &dir.join(name), "SVG view", Ok(content)).is_some();
            }
            if ok {
                artifacts.views_dir = Some(dir.to_string_lossy().into_owned());
            }
        }
        if let Some(path) = args.out_dxf {
            let content = render_dxf(&eval.solid, &eval.check_report);
            artifacts.dxf_path = export_text(&mut eval, &path, "DXF", content);
        }
        if let Some(path) = args.out_drawing {
            let parsed = cad_syntax::parse(&src);
            let ann = &parsed.file.annotations;
            let attribute = |path, name| annotation_string(ann, path, name);
            let pmi = drawing_parameter_pmi(&parsed.file.items, &eval);
            let content = render_drawing_svg(
                &eval.solid,
                &eval.check_report,
                DrawingOptions {
                    material: attribute("material", None),
                    manufacturing: attribute("manufacturing", Some("method")),
                    additional_pmi: &pmi,
                },
            );
            artifacts.drawing_path =
                export_text(&mut eval, &path, "engineering drawing", Ok(content));
        }
    }

    let output = build_cli_json(&file_str, &eval, Some(artifacts));
    print_output(&output, args.json);
    exit_code(eval.status_ok)
}

fn select_component(eval: &mut EvaluationOutput, kernel: &AnalyticKernel, id: &str) {
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

fn export_text(
    eval: &mut EvaluationOutput,
    path: &Path,
    format: &str,
    data: Result<String, String>,
) -> Option<String> {
    let write = |data| -> std::io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, data)
    };
    match data.and_then(|content| write(content).map_err(|e| e.to_string())) {
        Ok(()) => Some(path.to_string_lossy().into_owned()),
        Err(e) => {
            export_error(
                eval,
                format!("failed to export {format} to `{}`: {e}", path.display()),
            );
            None
        }
    }
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

fn run_lsp(file: &Path, cursor: Option<(u32, u32)>, json: bool) -> ExitCode {
    let file_str = file.to_string_lossy();
    let mut server = LspServerState::new();
    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            let message = format!("cannot read file `{file_str}`: {e}");
            if json {
                println!(
                    "{}",
                    serde_json::json!({"status":"error", "file":file_str, "diagnostics":[{
                    "code":"E001_IO_ERROR", "severity":1, "source":"cad-cli", "message":message,
                    "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}}
                }], "variables":[], "symbols":[], "completions":[], "hover":null})
                );
            } else {
                eprintln!("{message}");
            }
            return ExitCode::from(1);
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
        },
        components: eval.solid.components.clone(),
        artifacts,
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
            message: format!("cannot read file `{file_str}`: {msg}"),
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
        },
        components: Vec::new(),
        artifacts: None,
    };
    print_output(&output, json);
    ExitCode::from(1)
}

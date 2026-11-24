//! `cad-cli decompile` subcommand: Phase C B-Rep feature recognition and parametric
//! `CanonicalGraph` reconstruction with zero-trust symmetric volume verification closure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cad_decompiler::{
    DecompileOptions, DecompileReport, DecompileStatus, decompile_solid, save_decompiled_project,
};
use cad_evaluator::{FsAssetResolver, ImportOptions, evaluate_import_with_kernel};
use cad_hir::{Diagnostic, Severity};
use cad_kcl_bridge::EvalEngineMode;
use cad_kernel_occt::{GeometryKernel, SolidShape};
use cad_syntax::Span;
use clap::Args;
use serde::Serialize;

use super::{SourceArgs, evaluate_source_unified, file_parent, read_source};

#[derive(Args)]
pub(crate) struct DecompileArgs {
    /// Input model file (.kcl, .step, .stp, .brep, .iges, .igs, .stl, .3mf, .obj, .gltf, .glb)
    input: PathBuf,
    /// Optional path to write the decompiled SQLite .aicad project container
    #[arg(long, value_name = "PATH")]
    out_project: Option<PathBuf>,
    /// Optional path to write the emitted .kcl 2.0 source
    #[arg(long, value_name = "PATH")]
    out_kcl: Option<PathBuf>,
    /// Resolve imported assets relative to this workspace directory
    #[arg(long, value_name = "DIR")]
    workspace_root: Option<PathBuf>,
    /// Maximum allowed relative symmetric volume error for PASS (default: 0.001 = 0.1%)
    #[arg(long, default_value_t = 1e-3)]
    max_rel_error: f64,
    /// Emit structured JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct DecompileCliOutput {
    status: &'static str,
    input: String,
    part_name: String,
    kernel_backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    out_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    out_kcl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<DecompileReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<Diagnostic>,
}

fn diag(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: Severity::Error,
        message: message.into(),
        span: Span::default(),
    }
}

fn load_input_solid(
    args: &DecompileArgs,
    kernel: &dyn GeometryKernel,
) -> Result<SolidShape, Vec<Diagnostic>> {
    let ext = args
        .input
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    if ext == "kcl" {
        let source_args = SourceArgs {
            file: args.input.clone(),
            workspace_root: args.workspace_root.clone(),
        };
        let (src, resolver) =
            read_source(&source_args).map_err(|msg| vec![diag("E001_IO_ERROR", msg)])?;
        let ws_root = args
            .workspace_root
            .as_deref()
            .unwrap_or_else(|| file_parent(&args.input));
        let unified = evaluate_source_unified(&src, &resolver, Some(ws_root), kernel, EvalEngineMode::Auto);
        if !unified.output.status_ok {
            let diags = if unified.output.diagnostics.is_empty() {
                vec![diag(
                    "E207_INVALID_FEATURE",
                    "failed to evaluate input .kcl source",
                )]
            } else {
                unified.output.diagnostics
            };
            return Err(diags);
        }
        if unified.output.is_assembly {
            return Err(vec![diag(
                "E207_INVALID_FEATURE",
                "decompile expects a single solid part, not an assembly",
            )]);
        }
        return Ok(unified.output.solid);
    }

    let supported_import = matches!(
        ext.as_str(),
        "step" | "stp" | "brep" | "iges" | "igs" | "stl" | "3mf" | "obj" | "gltf" | "glb"
    );
    if !supported_import {
        return Err(vec![diag(
            "E504_UNSUPPORTED_IMPORT_FORMAT",
            format!("unsupported decompile input extension `.{ext}`"),
        )]);
    }

    let (root, rel_path) = if let Some(root) = args.workspace_root.as_deref() {
        (root, args.input.as_os_str())
    } else {
        let name = args
            .input
            .file_name()
            .ok_or_else(|| vec![diag("E502_UNSAFE_ASSET_PATH", "expected an asset filename")])?;
        (file_parent(&args.input), name)
    };
    let rel_str = rel_path
        .to_str()
        .ok_or_else(|| vec![diag("E502_UNSAFE_ASSET_PATH", "expected a UTF-8 asset path")])?;
    let resolver = FsAssetResolver::new(root.to_path_buf()).map_err(|e| {
        vec![diag(
            e.code(),
            format!("cannot open workspace root `{}`: {e}", root.display()),
        )]
    })?;
    let options = ImportOptions {
        allow_open_shell: true,
        ..Default::default()
    };
    let imported =
        evaluate_import_with_kernel(&resolver, rel_str, &options, Span::default(), kernel)
            .map_err(|d| vec![d])?;
    Ok(imported.solid)
}

fn write_kcl_file(path: &Path, content: &str) -> Result<(), Diagnostic> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| {
            diag(
                "E001_IO_ERROR",
                format!("cannot create directory `{}`: {e}", parent.display()),
            )
        })?;
    }
    fs::write(path, content).map_err(|e| {
        diag(
            "E001_IO_ERROR",
            format!("cannot write KCL file `{}`: {e}", path.display()),
        )
    })
}

pub(crate) fn run(args: DecompileArgs, kernel: &dyn GeometryKernel) -> ExitCode {
    let input_display = args.input.to_string_lossy().into_owned();
    let kernel_backend = kernel.backend_name().to_owned();

    if !args.max_rel_error.is_finite() || args.max_rel_error <= 0.0 {
        let out = DecompileCliOutput {
            status: "error",
            input: input_display,
            part_name: String::new(),
            kernel_backend,
            out_project: None,
            out_kcl: None,
            report: None,
            diagnostics: vec![diag(
                "E207_INVALID_FEATURE",
                "--max-rel-error must be finite and positive",
            )],
        };
        emit_output(&out, args.json);
        return ExitCode::from(1);
    }

    let solid = match load_input_solid(&args, kernel) {
        Ok(solid) => solid,
        Err(diagnostics) => {
            let is_io = diagnostics.iter().any(|d| d.code == "E001_IO_ERROR");
            let out = DecompileCliOutput {
                status: "error",
                input: input_display,
                part_name: String::new(),
                kernel_backend,
                out_project: None,
                out_kcl: None,
                report: None,
                diagnostics,
            };
            emit_output(&out, args.json);
            return ExitCode::from(if is_io { 2 } else { 1 });
        }
    };

    let options = DecompileOptions {
        max_relative_volume_error: args.max_rel_error,
        ..Default::default()
    };
    let decompiled = match decompile_solid(&solid, kernel, &options) {
        Ok(res) => res,
        Err(err) => {
            let out = DecompileCliOutput {
                status: "error",
                input: input_display,
                part_name: solid.part_name,
                kernel_backend,
                out_project: None,
                out_kcl: None,
                report: None,
                diagnostics: vec![diag("E207_INVALID_FEATURE", err.to_string())],
            };
            emit_output(&out, args.json);
            return ExitCode::from(1);
        }
    };

    let mut out_kcl_str = None;
    if let Some(kcl_path) = &args.out_kcl {
        if let Err(err) = write_kcl_file(kcl_path, &decompiled.kcl_source) {
            let out = DecompileCliOutput {
                status: "error",
                input: input_display,
                part_name: solid.part_name,
                kernel_backend,
                out_project: None,
                out_kcl: None,
                report: Some(decompiled.report),
                diagnostics: vec![err],
            };
            emit_output(&out, args.json);
            return ExitCode::from(2);
        }
        out_kcl_str = Some(kcl_path.to_string_lossy().into_owned());
    }

    let mut out_project_str = None;
    if let Some(project_path) = &args.out_project {
        if let Err(err) = save_decompiled_project(
            project_path,
            &decompiled.graph,
            &decompiled.kcl_source,
            kernel.backend_name(),
        ) {
            let out = DecompileCliOutput {
                status: "error",
                input: input_display,
                part_name: solid.part_name,
                kernel_backend,
                out_project: None,
                out_kcl: out_kcl_str,
                report: Some(decompiled.report),
                diagnostics: vec![diag("E001_IO_ERROR", err.to_string())],
            };
            emit_output(&out, args.json);
            return ExitCode::from(2);
        }
        out_project_str = Some(project_path.to_string_lossy().into_owned());
    }

    let status_str = match decompiled.report.status {
        DecompileStatus::Pass => "pass",
        DecompileStatus::Fail => "fail",
        DecompileStatus::Unknown => "unknown",
    };
    let passed = decompiled.report.status == DecompileStatus::Pass;

    let out = DecompileCliOutput {
        status: status_str,
        input: input_display,
        part_name: solid.part_name,
        kernel_backend,
        out_project: out_project_str,
        out_kcl: out_kcl_str,
        report: Some(decompiled.report),
        diagnostics: Vec::new(),
    };
    emit_output(&out, args.json);
    ExitCode::from(if passed { 0 } else { 1 })
}

fn emit_output(out: &DecompileCliOutput, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(out).unwrap());
        return;
    }
    for d in &out.diagnostics {
        eprintln!("[ERROR] {}: [{}] {}", out.input, d.code, d.message);
    }
    if let Some(report) = &out.report {
        println!(
            "[{}] {} ({} features, {} params, rel_vol_err={:.4}%, bbox_delta={:.4} mm, unrecognized_faces={})",
            out.status.to_uppercase(),
            out.input,
            report.recognized_features.len(),
            report.parameter_count,
            report.relative_volume_error * 100.0,
            report.bbox_max_delta_mm,
            report.unrecognized_face_count,
        );
        if let Some(kcl) = &out.out_kcl {
            println!("  wrote KCL: {kcl}");
        }
        if let Some(proj) = &out.out_project {
            println!("  wrote project: {proj}");
        }
        for note in &report.notes {
            eprintln!("  note: {note}");
        }
    }
}

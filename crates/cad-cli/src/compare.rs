use super::{SourceArgs, exit_code, read_source};
use cad_evaluator::{
    ImportFormat, ImportOptions, evaluate_import_with_kernel, evaluate_source_with_kernel,
};
use cad_hir::{Diagnostic, Severity};
use cad_kernel_occt::{GeometryKernel, KernelError, SymmetricDifference};
use cad_syntax::Span;
use clap::Args;
use serde::Serialize;
use std::process::ExitCode;

const THRESHOLD_RELATIVE_ERROR: f64 = 1e-3;

#[derive(Args)]
pub(crate) struct CompareArgs {
    #[command(flatten)]
    source: SourceArgs,
    /// Workspace-relative .step or .stp file containing one closed reference solid
    reference_step: String,
    /// Emit a machine-readable report; exit nonzero on mismatch or error
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct Metrics {
    method: &'static str,
    candidate_only_mm3: f64,
    reference_only_mm3: f64,
    reference_volume_mm3: f64,
    symmetric_difference_mm3: f64,
    relative_error: f64,
    threshold_relative_error: f64,
    threshold_operator: &'static str,
    passed: bool,
}

impl From<SymmetricDifference> for Metrics {
    fn from(value: SymmetricDifference) -> Self {
        Self {
            method: "occt_brep_symmetric_difference",
            candidate_only_mm3: value.candidate_only_mm3,
            reference_only_mm3: value.reference_only_mm3,
            reference_volume_mm3: value.reference_volume_mm3,
            symmetric_difference_mm3: value.symmetric_difference_mm3,
            relative_error: value.relative_error,
            threshold_relative_error: THRESHOLD_RELATIVE_ERROR,
            threshold_operator: "<",
            passed: value.relative_error < THRESHOLD_RELATIVE_ERROR,
        }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    status: &'static str,
    file: &'a str,
    reference_step: &'a str,
    diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<Metrics>,
}

fn diagnostic(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: message.into(),
        span: Span::default(),
        severity: Severity::Error,
    }
}

fn kernel_diagnostic(error: KernelError) -> Diagnostic {
    let code = match error {
        KernelError::BackendUnavailable => "E504_UNSUPPORTED_IMPORT_FORMAT",
        KernelError::OpenShell => "E505_IMPORT_NON_MANIFOLD_OR_OPEN",
        KernelError::UnsupportedMixedBoolean => "E507_UNSUPPORTED_MIXED_BOOLEAN",
        KernelError::OperationFailed(_) => "E207_INVALID_FEATURE",
    };
    diagnostic(code, format!("Cannot compare exact solids: {error}"))
}

pub(crate) fn run(args: CompareArgs, kernel: &dyn GeometryKernel) -> ExitCode {
    let file = args.source.file.to_string_lossy();
    let mut report = Report {
        status: "error",
        file: &file,
        reference_step: &args.reference_step,
        diagnostics: Vec::new(),
        metrics: None,
    };
    let comparison = (|| {
        let (src, resolver) =
            read_source(&args.source).map_err(|message| diagnostic("E001_IO_ERROR", message))?;
        let evaluated = evaluate_source_with_kernel(&src, &resolver, kernel);
        report.diagnostics = evaluated.diagnostics;
        if !evaluated.status_ok {
            return Ok(None);
        }
        if evaluated.is_assembly {
            return Err(diagnostic(
                "E207_INVALID_FEATURE",
                "Exact comparison requires one closed solid, not an assembly",
            ));
        }
        if ImportFormat::from_path(&args.reference_step) != Some(ImportFormat::Step) {
            return Err(diagnostic(
                "E504_UNSUPPORTED_IMPORT_FORMAT",
                "Reference must have a .step or .stp extension",
            ));
        }
        let reference = evaluate_import_with_kernel(
            &resolver,
            &args.reference_step,
            &ImportOptions::default(),
            Span::default(),
            kernel,
        )?;
        kernel.measure_symmetric_difference(&evaluated.solid, &reference.solid)
            .map(|result| Some(Metrics::from(result)))
            .map_err(kernel_diagnostic)
    })();
    match comparison {
        Ok(Some(metrics)) => {
            report.status = if metrics.passed { "ok" } else { "mismatch" };
            report.metrics = Some(metrics);
        }
        Ok(None) => {}
        Err(error) => report.diagnostics.push(error),
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        for diagnostic in &report.diagnostics {
            eprintln!("{}: {}", diagnostic.code, diagnostic.message);
        }
        if let Some(metrics) = report.metrics {
            println!(
                "{}: relative symmetric difference {:.9}% (required < {:.3}%)",
                report.status,
                metrics.relative_error * 100.0,
                metrics.threshold_relative_error * 100.0,
            );
        }
    }
    if let Some(io) = report.diagnostics.iter().find(|d| d.code == "E001_IO_ERROR") {
        eprintln!("error: {}", io.message);
        tracing::error!(code = cad_observe::codes::R016_IO_ERROR, error_type = "IoError", "{}", io.message);
        return ExitCode::from(2);
    }
    exit_code(report.status == "ok")
}

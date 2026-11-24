use cad_hir::{Diagnostic, check_and_lower};
use cad_ir::{CanonicalGraph, IrValue};
use cad_kcl_bridge::{
    KclSourceFlavor, bridge_error_diagnostic, diagnostic_error, inspect_graph_eval_readiness,
};
use cad_kernel_occt::{
    BBox3, CheckReport, ComponentMeta, GeometryKernel, KernelError, KernelPayload, Mat4,
    SolidShape, default_brep_kernel,
};
use cad_scene_mesh::{EditHandle, SceneMeshPayload, build_scene_mesh};
use cad_syntax::parse;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use handles::hole_offset_handles;

mod asset_resolver;
mod eval_diagnostics;
pub mod extref_eval;
pub mod graph_eval;
mod handles;
mod import_geometry;
pub mod merkle_cache;
pub mod sheet_metal_unfold;
mod sketch_sample;
pub mod xcaf_import;

pub use asset_resolver::{
    AssetResolveError, AssetResolver, FsAssetResolver, NoopAssetResolver, asset_sha256,
};
pub use cad_gcs::SolveStatus;
pub use cad_mesh_io::LengthUnit;
pub use extref_eval::DeferredComponentSummary;
pub use graph_eval::{
    GraphEvalCache, GraphEvalReport, GraphEvaluator, NodeEvalReport, NodeSnapshot,
    SketchEvalSummary, graph_report_to_evaluation_output,
};
pub use import_geometry::{
    EvaluatedImport, ImportFormat, ImportOptions, evaluate_import, evaluate_import_with_kernel,
};
pub use merkle_cache::MerkleEvalCache;
pub use sheet_metal_unfold::{
    BendLine2D, SheetMetalEdgeSide, SheetMetalFlangeSpec, SheetMetalPartSpec, UnfoldedSheetPattern,
    unfold_sheet_metal_part,
};
pub use sketch_sample::{sample_closed_spline_polygon, sample_helix_path, sample_spline3d_path};
pub use xcaf_import::{merge_xcaf_into_project, xcaf_metadata_to_engineering};

const FACE_EXTREME_TOLERANCE: f64 = 1e-4;

#[derive(Debug, Clone)]
pub struct EvaluationOutput {
    pub status_ok: bool,
    pub part_name: String,
    pub is_assembly: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub check_report: CheckReport,
    pub solid: SolidShape,
    /// Components in part-local coordinates, keyed by the ID in the assembly BOM.
    pub component_solids: HashMap<String, SolidShape>,
    pub scene_mesh: SceneMeshPayload,
    pub compile_time_ms: f64,
    pub kernel_backend: String,
}

impl EvaluationOutput {
    /// Get an assembly instance in the same coordinate system as the evaluated model.
    pub fn placed_component(&self, component: &ComponentMeta) -> Result<SolidShape, KernelError> {
        let kernel = default_brep_kernel();
        self.placed_component_with_kernel(component, kernel.as_ref())
    }

    pub fn placed_component_with_kernel(
        &self,
        component: &ComponentMeta,
        kernel: &dyn GeometryKernel,
    ) -> Result<SolidShape, KernelError> {
        let shape = self
            .component_solids
            .get(&component.component_id)
            .ok_or_else(|| {
                KernelError::OperationFailed(format!(
                    "missing geometry for {}",
                    component.component_id
                ))
            })?;
        let world = placement(component.origin_mm, component.rotation_deg);
        kernel.transform(shape, world)
    }
}

pub(crate) fn is_imported_preview(solid: &SolidShape) -> bool {
    !matches!(solid.payload, KernelPayload::Analytic)
        && solid
            .faces
            .first()
            .is_some_and(|f| f.face_id.starts_with("import_preview_"))
}

pub(crate) fn placement(origin: [f64; 3], rotation: [f64; 3]) -> Mat4 {
    Mat4::translate(origin[0], origin[1], origin[2]).mul_mat(Mat4::rotate_euler_deg(
        rotation[0],
        rotation[1],
        rotation[2],
    ))
}

/// Evaluate KCL / CAD-DSL source via `CanonicalGraph` + `GraphEvaluator`.
pub fn evaluate_source(source: &str) -> EvaluationOutput {
    evaluate_source_with_resolver(source, &NoopAssetResolver)
}

/// Evaluate source with an explicit resolver for workspace-relative assets.
pub fn evaluate_source_with_resolver(
    source: &str,
    resolver: &dyn AssetResolver,
) -> EvaluationOutput {
    let kernel = default_brep_kernel();
    evaluate_source_with_kernel(source, resolver, kernel.as_ref())
}

/// Evaluate source with a caller-supplied geometry kernel and asset resolver.
pub fn evaluate_source_with_kernel(
    source: &str,
    resolver: &dyn AssetResolver,
    kernel: &dyn GeometryKernel,
) -> EvaluationOutput {
    evaluate_source_with_kernel_and_workspace(source, resolver, None, kernel)
}

/// Evaluate source with a caller-supplied geometry kernel, asset resolver, and
/// optional workspace sandbox root for `ExternalRef` resolution.
pub fn evaluate_source_with_kernel_and_workspace(
    source: &str,
    resolver: &dyn AssetResolver,
    workspace_root: Option<&Path>,
    kernel: &dyn GeometryKernel,
) -> EvaluationOutput {
    let t0 = Instant::now();
    let span = tracing::info_span!(
        "evaluate_source",
        component = "cad-evaluator",
        operation = "evaluate_source"
    );
    let _entered = span.enter();

    let readiness = inspect_graph_eval_readiness(source);
    let Some(graph) = readiness.graph.as_ref().filter(|_| readiness.is_graph_ready()) else {
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
        let output = empty_error_output(part_name, diagnostics, kernel, t0);
        record_source_eval_summary(&output);
        return output;
    };

    let output = evaluate_graph_source_output(
        source,
        readiness.flavor,
        graph,
        resolver,
        workspace_root,
        kernel,
        t0,
    );
    record_source_eval_summary(&output);
    output
}

/// Evaluate a pre-lowered `CanonicalGraph` into [`EvaluationOutput`], attaching
/// CAD-DSL edit handles and direct-edit hole offsets when `flavor == DslV02`.
pub fn evaluate_graph_source_output(
    source: &str,
    flavor: KclSourceFlavor,
    graph: &CanonicalGraph,
    resolver: &dyn AssetResolver,
    workspace_root: Option<&Path>,
    kernel: &dyn GeometryKernel,
    started: Instant,
) -> EvaluationOutput {
    let mut evaluator = GraphEvaluator::with_resolver(kernel, resolver);
    if let Some(ws) = workspace_root {
        evaluator.set_external_ref_roots(None, Some(ws.to_path_buf()));
    }
    let report = match evaluator.evaluate(graph) {
        Ok(report) => report,
        Err(err) => {
            return empty_error_output(
                graph.graph_id.clone(),
                vec![diagnostic_error("E002_SYNTAX_ERROR", err.to_string())],
                kernel,
                started,
            );
        }
    };
    let mut output = graph_report_to_evaluation_output(
        graph,
        report,
        kernel,
        started.elapsed().as_secs_f64() * 1000.0,
    );
    if flavor == KclSourceFlavor::DslV02 {
        enrich_dsl_scene_handles(source, graph, kernel, &mut output);
    }
    output
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

fn enrich_dsl_scene_handles(
    source: &str,
    graph: &CanonicalGraph,
    kernel: &dyn GeometryKernel,
    output: &mut EvaluationOutput,
) {
    let parsed = parse(source.trim_start_matches('\u{feff}'));
    if !parsed.lex_errors.is_empty() || !parsed.syntax_errors.is_empty() {
        return;
    }
    let hir = check_and_lower(&parsed.file);
    let target_ir = if !hir.module.assemblies.is_empty() {
        hir.module.assemblies.last()
    } else {
        hir.module.parts.last()
    };
    let Some(target_ir) = target_ir else {
        return;
    };
    let mut resolved_graph = graph.clone();
    let _ = resolved_graph.evaluate_formulas();
    output.scene_mesh.edit_handles = target_ir
        .edit_handles
        .iter()
        .filter_map(|h| {
            let val = resolved_graph
                .parameters
                .get(&h.param_name)
                .and_then(|p| match &p.value {
                    IrValue::Quantity(q) => Some(q.canonical_value()),
                    IrValue::Scalar(v) => Some(*v),
                    IrValue::Int(v) => Some(*v as f64),
                    _ => None,
                })
                .or(h.current_value)?;
            Some(EditHandle {
                handle_id: h.handle_id.clone(),
                param_name: h.param_name.clone(),
                tier: h.tier,
                current_value: val,
                unit: if h.unit.is_empty() {
                    "unitless".to_string()
                } else {
                    h.unit.clone()
                },
                ast_span: h.ast_span,
                upstream_param: h.classification.base_param.clone(),
                offset_value: h.classification.offset_value,
            })
        })
        .collect();
    if output.status_ok
        && output.check_report.watertight
        && !output.is_assembly
        && let Some(node) = target_ir
            .nodes
            .iter()
            .find(|node| Some(node.id) == target_ir.return_node)
    {
        output.scene_mesh.hole_offsets = hole_offset_handles(kernel, &output.solid, node.span);
    }
}

/// Extract Tier 1/2/3 AST edit handles from a KCL source file, resolving live
/// parameter values from the evaluated `CanonicalGraph` when available.
pub fn extract_source_edit_handles(
    source: &str,
    graph: &cad_ir::CanonicalGraph,
) -> Vec<EditHandle> {
    let parsed = parse(source);
    if !parsed.lex_errors.is_empty() || !parsed.syntax_errors.is_empty() {
        return Vec::new();
    }
    let hir = check_and_lower(&parsed.file);
    let Some(target_ir) = hir
        .module
        .assemblies
        .last()
        .or_else(|| hir.module.parts.last())
    else {
        return Vec::new();
    };
    target_ir
        .edit_handles
        .iter()
        .filter_map(|h| {
            let graph_val = graph.parameters.get(&h.param_name).and_then(|def| {
                match &def.value {
                    cad_ir::IrValue::Quantity(q) => Some(q.canonical_value()),
                    cad_ir::IrValue::Scalar(v) => Some(*v),
                    cad_ir::IrValue::Int(i) => Some(*i as f64),
                    _ => None,
                }
            });
            let val = graph_val.or(h.current_value)?;
            Some(EditHandle {
                handle_id: h.handle_id.clone(),
                param_name: h.param_name.clone(),
                tier: h.tier,
                current_value: val,
                unit: if h.unit.is_empty() {
                    "unitless".to_string()
                } else {
                    h.unit.clone()
                },
                ast_span: h.ast_span,
                upstream_param: h.classification.base_param.clone(),
                offset_value: h.classification.offset_value,
            })
        })
        .collect()
}

fn record_source_eval_summary(output: &EvaluationOutput) {
    tracing::info!(
        component = "cad-evaluator",
        operation = "evaluate_source",
        duration_ms = output.compile_time_ms,
        status_ok = output.status_ok,
        diagnostics = output.diagnostics.len(),
        "evaluate_source finished: status_ok={} diagnostics={}",
        output.status_ok,
        output.diagnostics.len(),
    );
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}

fn delta(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}

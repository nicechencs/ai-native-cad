//! Parametric [`CanonicalGraph`] reconstruction and zero-trust symmetric volume verification closure.

use std::path::Path;

use cad_container::{AicadProject, ContainerError, ContainerManifest, KernelFingerprint, SqliteContainer};
use cad_evaluator::GraphEvaluator;
use cad_ir::{
    BooleanOpKind, CanonicalGraph, DimInput, FeatureSpec, IntInput, IrError, IrNode, IrType,
    IrValue, LengthUnit, LinearPatternAxisSpec, NodeId, NodeState, ParamId, ParameterDef,
    Quantity, validate_graph,
};
use cad_kcl_bridge::{BridgeError, emit_kcl};
use cad_kernel_occt::{BBox3, CsgNode, GeometryKernel, KernelError, SolidShape};
use serde::{Deserialize, Serialize};

use crate::{
    aag::{AttributedAdjacencyGraph, Vec3, add, scale, sub},
    recognize::{
        BaseStock, DefaultFeatureRecognizer, DressUpHint, DressUpHintKind, FeatureRecognizer,
        HoleFeature, HoleKind, HolePatternFeature, PatternKind, PocketFeature, RecognitionOptions,
        RecognitionResult, RecognizedFeature,
    },
};

const CUT_OVERSHOOT_MM: f64 = 1.0;

/// Options controlling decompilation and zero-trust verification thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecompileOptions {
    pub graph_id: Option<String>,
    pub recognition: RecognitionOptions,
    /// Maximum allowed relative symmetric volume error for `PASS` (default: `0.001` = 0.1%).
    pub max_relative_volume_error: f64,
    /// Maximum allowed bounding box coordinate delta in millimetres for `PASS` (default: `0.05` mm).
    pub max_bbox_delta_mm: f64,
    /// Lattice resolution per axis when falling back to analytic occupancy symmetric difference.
    pub analytic_sampling_resolution: usize,
}

impl Default for DecompileOptions {
    fn default() -> Self {
        Self {
            graph_id: None,
            recognition: RecognitionOptions::default(),
            max_relative_volume_error: 1e-3,
            max_bbox_delta_mm: 0.05,
            analytic_sampling_resolution: 32,
        }
    }
}

/// Tri-state zero-trust verification verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DecompileStatus {
    Pass,
    Fail,
    Unknown,
}

/// Verification method used to measure the symmetric volume error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    OcctSymmetricDifference,
    AnalyticVolumeAndSampling,
    EvaluationFailed,
}

/// Structured zero-trust verification report for a decompiled solid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecompileReport {
    pub status: DecompileStatus,
    pub verification_method: VerificationMethod,
    pub original_volume_mm3: f64,
    pub reconstructed_volume_mm3: f64,
    pub symmetric_difference_mm3: f64,
    pub relative_volume_error: f64,
    pub max_relative_volume_error: f64,
    pub bbox_max_delta_mm: f64,
    pub centroid_delta_mm: f64,
    pub original_watertight: bool,
    pub reconstructed_watertight: bool,
    pub recognized_features: Vec<RecognizedFeature>,
    pub dressup_hints: Vec<DressUpHint>,
    pub total_face_count: usize,
    pub recognized_face_count: usize,
    pub unrecognized_face_count: usize,
    pub parameter_count: usize,
    pub node_count: usize,
    pub notes: Vec<String>,
}

/// Complete decompilation artifact bundle.
#[derive(Debug, Clone)]
pub struct DecompileOutput {
    pub aag: AttributedAdjacencyGraph,
    pub recognition: RecognitionResult,
    pub graph: CanonicalGraph,
    pub kcl_source: String,
    pub report: DecompileReport,
}

/// Errors produced during decompilation, graph emission, or container persistence.
#[derive(Debug, thiserror::Error)]
pub enum DecompileError {
    #[error("no prismatic base stock could be recognized from the input solid")]
    NoBaseStockRecognized,
    #[error("IR construction or validation failed: {0}")]
    Ir(#[from] IrError),
    #[error("KCL 2.0 emission failed: {0}")]
    Bridge(#[from] BridgeError),
    #[error("project container save failed: {0}")]
    Container(#[from] ContainerError),
}

/// Run the full Phase C decompilation pipeline using [`DefaultFeatureRecognizer`]:
/// `SolidShape` -> `AttributedAdjacencyGraph` -> feature recognition -> `CanonicalGraph`
/// -> `.kcl 2.0` -> zero-trust `verify_decompilation`.
pub fn decompile_solid(
    solid: &SolidShape,
    kernel: &dyn GeometryKernel,
    options: &DecompileOptions,
) -> Result<DecompileOutput, DecompileError> {
    decompile_solid_with_recognizer(solid, kernel, &DefaultFeatureRecognizer, options)
}

/// Run the full Phase C decompilation pipeline with a custom pluggable [`FeatureRecognizer`].
pub fn decompile_solid_with_recognizer(
    solid: &SolidShape,
    kernel: &dyn GeometryKernel,
    recognizer: &dyn FeatureRecognizer,
    options: &DecompileOptions,
) -> Result<DecompileOutput, DecompileError> {
    let aag = AttributedAdjacencyGraph::from_solid(solid);
    let recognition = recognizer.recognize(&aag, &options.recognition);
    let graph_id = options
        .graph_id
        .clone()
        .unwrap_or_else(|| sanitize_graph_id(&solid.part_name));

    let (graph, kcl_source, report) = match reconstruct_canonical_graph(&graph_id, &recognition) {
        Ok(graph) => {
            let kcl_source = emit_kcl(&graph)?;
            let report = verify_decompilation(solid, &graph, &aag, &recognition, kernel, options);
            (graph, kcl_source, report)
        }
        Err(DecompileError::NoBaseStockRecognized) => {
            // Return an honest FAIL report rather than crashing so callers (e.g. CLI) receive
            // structured diagnostics on freeform/non-prismatic solids.
            let empty_graph = CanonicalGraph::new(&graph_id);
            let kcl_source = emit_kcl(&empty_graph)?;
            let report = DecompileReport {
                status: DecompileStatus::Fail,
                verification_method: VerificationMethod::EvaluationFailed,
                original_volume_mm3: aag.solid_volume_mm3,
                reconstructed_volume_mm3: 0.0,
                symmetric_difference_mm3: aag.solid_volume_mm3,
                relative_volume_error: 1.0,
                max_relative_volume_error: options.max_relative_volume_error,
                bbox_max_delta_mm: f64::INFINITY,
                centroid_delta_mm: f64::INFINITY,
                original_watertight: aag.is_watertight,
                reconstructed_watertight: false,
                recognized_features: recognition.features(),
                dressup_hints: recognition.dressup_hints.clone(),
                total_face_count: aag.faces.len(),
                recognized_face_count: recognition.explained_face_ids.len(),
                unrecognized_face_count: recognition.unrecognized_face_ids.len(),
                parameter_count: 0,
                node_count: 0,
                notes: vec![
                    "No prismatic base stock (BaseBox or BaseCylinder) could be recognized; solid appears to be freeform or non-prismatic.".to_owned(),
                ],
            };
            (empty_graph, kcl_source, report)
        }
        Err(other) => return Err(other),
    };

    Ok(DecompileOutput {
        aag,
        recognition,
        graph,
        kcl_source,
        report,
    })
}

/// Convert recognized prismatic features into a validated, editable [`CanonicalGraph`]
/// with clean named `ParameterDef` entries and typed `FeatureSpec` nodes.
pub fn reconstruct_canonical_graph(
    graph_id: &str,
    recognition: &RecognitionResult,
) -> Result<CanonicalGraph, DecompileError> {
    let base = recognition
        .base_stock
        .as_ref()
        .ok_or(DecompileError::NoBaseStockRecognized)?;

    let mut graph = CanonicalGraph::new(sanitize_graph_id(graph_id));

    // 1. Base stock node.
    let mut current_node = match base {
        BaseStock::BaseBox {
            origin_mm, size_mm, ..
        } => {
            add_length_param(&mut graph, "base_width", size_mm[0])?;
            add_length_param(&mut graph, "base_depth", size_mm[1])?;
            add_length_param(&mut graph, "base_height", size_mm[2])?;
            let node = IrNode::from_feature_spec(
                "n_base_stock",
                "base_stock",
                FeatureSpec::PrimitiveBox {
                    width: DimInput::param("base_width"),
                    depth: DimInput::param("base_depth"),
                    height: DimInput::param("base_height"),
                    origin_mm: round_vec3(*origin_mm),
                },
            )?;
            insert_node(&mut graph, node)
        }
        BaseStock::BaseCylinder {
            origin_mm,
            axis,
            radius_mm,
            height_mm,
            ..
        } => {
            add_length_param(&mut graph, "base_radius", *radius_mm)?;
            add_length_param(&mut graph, "base_height", *height_mm)?;
            let node = IrNode::from_feature_spec(
                "n_base_stock",
                "base_stock",
                FeatureSpec::PrimitiveCylinder {
                    radius: DimInput::param("base_radius"),
                    height: DimInput::param("base_height"),
                    origin_mm: round_vec3(*origin_mm),
                    axis: round_vec3(*axis),
                },
            )?;
            insert_node(&mut graph, node)
        }
    };

    // 2. Uniform base stock dress-up (corner fillet or chamfer applied prior to boolean features).
    for hint in &recognition.dressup_hints {
        if !hint.uniform_base_dressup {
            continue;
        }
        match hint.kind {
            DressUpHintKind::Fillet => {
                let param_name = "base_fillet_radius";
                if !graph.parameters.contains_key(param_name) {
                    add_length_param(&mut graph, param_name, hint.radius_or_distance_mm)?;
                    let node = IrNode::from_feature_spec(
                        "n_base_fillet",
                        "base_fillet",
                        FeatureSpec::Fillet {
                            solid: current_node,
                            edges: Vec::new(),
                            radius: DimInput::param(param_name),
                        },
                    )?;
                    current_node = insert_node(&mut graph, node);
                }
            }
            DressUpHintKind::Chamfer => {
                let param_name = "base_chamfer_distance";
                if !graph.parameters.contains_key(param_name) {
                    add_length_param(&mut graph, param_name, hint.radius_or_distance_mm)?;
                    let node = IrNode::from_feature_spec(
                        "n_base_chamfer",
                        "base_chamfer",
                        FeatureSpec::Chamfer {
                            solid: current_node,
                            edges: Vec::new(),
                            distance: DimInput::param(param_name),
                            distance2: None,
                            angle: None,
                        },
                    )?;
                    current_node = insert_node(&mut graph, node);
                }
            }
        }
    }

    // 3. Boss additions (PrimitiveCylinder + BooleanUnion).
    if !recognition.bosses.is_empty() {
        let mut boss_nodes = Vec::with_capacity(recognition.bosses.len());
        for (idx, boss) in recognition.bosses.iter().enumerate() {
            let k = idx + 1;
            let r_param = format!("boss_{k}_radius");
            let h_param = format!("boss_{k}_height");
            add_length_param(&mut graph, &r_param, 0.5 * boss.diameter_mm)?;
            add_length_param(&mut graph, &h_param, boss.height_mm)?;
            let node = IrNode::from_feature_spec(
                format!("n_boss_{k}"),
                format!("boss_{k}"),
                FeatureSpec::PrimitiveCylinder {
                    radius: DimInput::param(r_param),
                    height: DimInput::param(h_param),
                    origin_mm: round_vec3(boss.base_center_mm),
                    axis: round_vec3(boss.axis),
                },
            )?;
            boss_nodes.push(insert_node(&mut graph, node));
        }
        let union_node = IrNode::from_feature_spec(
            "n_boss_union",
            "boss_union",
            FeatureSpec::Boolean {
                op: BooleanOpKind::Union,
                target: current_node,
                tools: boss_nodes,
            },
        )?;
        current_node = insert_node(&mut graph, union_node);
    }

    // 4. Subtract pockets sequentially (shallowest first) so stepped pockets punch
    //    cleanly into upstream pocket floors in both Analytic and OCCT kernels.
    for (idx, pocket) in recognition.pockets.iter().enumerate() {
        let k = idx + 1;
        let tool_id = emit_pocket_tool(&mut graph, k, pocket)?;
        let sub_node = IrNode::from_feature_spec(
            format!("n_pocket_{k}_subtract"),
            format!("pocket_{k}_subtract"),
            FeatureSpec::Boolean {
                op: BooleanOpKind::Subtract,
                target: current_node,
                tools: vec![tool_id],
            },
        )?;
        current_node = insert_node(&mut graph, sub_node);
    }

    let mut hole_tools = Vec::new();
    for (idx, hole) in recognition.holes.iter().enumerate() {
        let k = idx + 1;
        let tool_id = emit_hole_tool(&mut graph, &format!("hole_{k}"), hole)?;
        hole_tools.push(tool_id);
    }
    for (idx, pat) in recognition.hole_patterns.iter().enumerate() {
        let k = idx + 1;
        let pat_id = emit_hole_pattern_tool(&mut graph, k, pat)?;
        hole_tools.push(pat_id);
    }
    if !hole_tools.is_empty() {
        let sub_node = IrNode::from_feature_spec(
            "n_hole_subtract",
            "hole_subtract",
            FeatureSpec::Boolean {
                op: BooleanOpKind::Subtract,
                target: current_node,
                tools: hole_tools,
            },
        )?;
        current_node = insert_node(&mut graph, sub_node);
    }

    graph.outputs = vec![current_node];
    validate_graph(&graph)?;
    Ok(graph)
}

fn emit_pocket_tool(
    graph: &mut CanonicalGraph,
    index: usize,
    pocket: &PocketFeature,
) -> Result<NodeId, DecompileError> {
    let mut origin = pocket.origin_mm;
    let mut size = pocket.size_mm;

    // Extend cutter slightly past open face(s) to guarantee clean boolean subtraction.
    let open_axis = (0..3)
        .max_by(|&a, &b| {
            pocket.opening_normal[a]
                .abs()
                .total_cmp(&pocket.opening_normal[b].abs())
        })
        .unwrap_or(2);
    if pocket.is_through {
        origin[open_axis] -= CUT_OVERSHOOT_MM;
        size[open_axis] += 2.0 * CUT_OVERSHOOT_MM;
    } else if pocket.opening_normal[open_axis] > 0.0 {
        size[open_axis] += CUT_OVERSHOOT_MM;
    } else {
        origin[open_axis] -= CUT_OVERSHOOT_MM;
        size[open_axis] += CUT_OVERSHOOT_MM;
    }

    let w_param = format!("pocket_{index}_width");
    let d_param = format!("pocket_{index}_depth");
    let h_param = format!("pocket_{index}_height");
    add_length_param(graph, &w_param, size[0])?;
    add_length_param(graph, &d_param, size[1])?;
    add_length_param(graph, &h_param, size[2])?;

    let node = IrNode::from_feature_spec(
        format!("n_pocket_{index}"),
        format!("pocket_{index}"),
        FeatureSpec::PrimitiveBox {
            width: DimInput::param(w_param),
            depth: DimInput::param(d_param),
            height: DimInput::param(h_param),
            origin_mm: round_vec3(origin),
        },
    )?;
    Ok(insert_node(graph, node))
}

fn emit_hole_tool(
    graph: &mut CanonicalGraph,
    prefix: &str,
    hole: &HoleFeature,
) -> Result<NodeId, DecompileError> {
    let r_param = format!("{prefix}_radius");
    let d_param = format!("{prefix}_depth");
    let is_through = matches!(hole.kind, HoleKind::ThroughHole | HoleKind::CounterboreThrough);
    let start_origin = sub(hole.entry_center_mm, scale(hole.axis, CUT_OVERSHOOT_MM));
    let shaft_height = if is_through {
        hole.depth_mm + 2.0 * CUT_OVERSHOOT_MM
    } else {
        hole.depth_mm + CUT_OVERSHOOT_MM
    };

    add_length_param(graph, &r_param, 0.5 * hole.diameter_mm)?;
    add_length_param(graph, &d_param, shaft_height)?;

    let shaft_node = IrNode::from_feature_spec(
        format!("n_{prefix}_shaft"),
        format!("{prefix}_shaft"),
        FeatureSpec::PrimitiveCylinder {
            radius: DimInput::param(r_param),
            height: DimInput::param(d_param),
            origin_mm: round_vec3(start_origin),
            axis: round_vec3(hole.axis),
        },
    )?;
    let shaft_id = insert_node(graph, shaft_node);

    if let (Some(cb_diam), Some(cb_depth)) =
        (hole.counterbore_diameter_mm, hole.counterbore_depth_mm)
    {
        let cb_r_param = format!("{prefix}_cbore_radius");
        let cb_d_param = format!("{prefix}_cbore_depth");
        add_length_param(graph, &cb_r_param, 0.5 * cb_diam)?;
        add_length_param(graph, &cb_d_param, cb_depth + CUT_OVERSHOOT_MM)?;
        let cb_node = IrNode::from_feature_spec(
            format!("n_{prefix}_cbore"),
            format!("{prefix}_cbore"),
            FeatureSpec::PrimitiveCylinder {
                radius: DimInput::param(cb_r_param),
                height: DimInput::param(cb_d_param),
                origin_mm: round_vec3(start_origin),
                axis: round_vec3(hole.axis),
            },
        )?;
        let cb_id = insert_node(graph, cb_node);
        let union_node = IrNode::from_feature_spec(
            format!("n_{prefix}_tool"),
            format!("{prefix}_tool"),
            FeatureSpec::Boolean {
                op: BooleanOpKind::Union,
                target: shaft_id,
                tools: vec![cb_id],
            },
        )?;
        Ok(insert_node(graph, union_node))
    } else {
        Ok(shaft_id)
    }
}

fn emit_hole_pattern_tool(
    graph: &mut CanonicalGraph,
    index: usize,
    pat: &HolePatternFeature,
) -> Result<NodeId, DecompileError> {
    let prefix = format!("pattern_{index}_hole");
    let seed_tool = emit_hole_tool(graph, &prefix, &pat.seed_hole)?;

    match &pat.pattern {
        PatternKind::PolarPattern {
            axis_origin_mm,
            axis_direction,
            count,
            total_angle_deg,
            ..
        } => {
            let count_param = format!("pattern_{index}_count");
            add_int_param(graph, &count_param, *count as i64);
            let pat_node = IrNode::from_feature_spec(
                format!("n_hole_pattern_{index}"),
                format!("hole_pattern_{index}"),
                FeatureSpec::PolarPattern {
                    solid: seed_tool,
                    axis_origin_mm: round_vec3(*axis_origin_mm),
                    axis_direction: round_vec3(*axis_direction),
                    count: IntInput::param(count_param),
                    total_angle: DimInput::deg(round_clean(*total_angle_deg))?,
                    radius: None,
                    start_angle: None,
                },
            )?;
            Ok(insert_node(graph, pat_node))
        }
        PatternKind::LinearPattern {
            direction,
            count,
            spacing_mm,
            second_direction,
            second_count,
            second_spacing_mm,
        } => {
            let count1_param = format!("pattern_{index}_count");
            let spacing1_param = format!("pattern_{index}_spacing");
            add_int_param(graph, &count1_param, *count as i64);
            add_length_param(graph, &spacing1_param, *spacing_mm)?;

            let second = if let (Some(dir2), Some(c2), Some(s2)) =
                (second_direction, second_count, second_spacing_mm)
            {
                let count2_param = format!("pattern_{index}_count_2");
                let spacing2_param = format!("pattern_{index}_spacing_2");
                add_int_param(graph, &count2_param, *c2 as i64);
                add_length_param(graph, &spacing2_param, *s2)?;
                Some(LinearPatternAxisSpec {
                    direction: round_vec3(*dir2),
                    count: IntInput::param(count2_param),
                    spacing: DimInput::param(spacing2_param),
                })
            } else {
                None
            };

            let pat_node = IrNode::from_feature_spec(
                format!("n_hole_pattern_{index}"),
                format!("hole_pattern_{index}"),
                FeatureSpec::LinearPattern {
                    solid: seed_tool,
                    direction: round_vec3(*direction),
                    count: IntInput::param(count1_param),
                    spacing: DimInput::param(spacing1_param),
                    second_direction: second,
                },
            )?;
            Ok(insert_node(graph, pat_node))
        }
    }
}

/// Zero-trust verification closure: evaluate the reconstructed [`CanonicalGraph`] and compare
/// against the original [`SolidShape`] by exact OCCT symmetric difference (when available) or
/// combined exact solid volume + deterministic 3D lattice occupancy symmetric difference + bbox
/// & centroid parity.
pub fn verify_decompilation(
    original: &SolidShape,
    graph: &CanonicalGraph,
    aag: &AttributedAdjacencyGraph,
    recognition: &RecognitionResult,
    kernel: &dyn GeometryKernel,
    options: &DecompileOptions,
) -> DecompileReport {
    let mut notes = Vec::new();
    let orig_check = kernel.check_solid(original);
    let orig_vol = orig_check.volume_mm3.max(0.0);
    let orig_bbox = if orig_check.bbox_mm.is_valid() {
        orig_check.bbox_mm
    } else {
        original.csg.bbox()
    };

    let mut evaluator = GraphEvaluator::new(kernel);
    let eval_report = match evaluator.evaluate(graph) {
        Ok(r) => r,
        Err(err) => {
            notes.push(format!("Graph evaluation error: {err}"));
            return failed_report(
                orig_vol,
                orig_check.watertight,
                aag,
                recognition,
                graph,
                options,
                notes,
            );
        }
    };

    let failed_nodes: Vec<String> = eval_report
        .node_reports
        .iter()
        .filter_map(|(id, nr)| {
            if let NodeState::FailedWithRollback { reason, .. } = &nr.status {
                Some(format!("{id}: {reason}"))
            } else {
                None
            }
        })
        .collect();
    if !failed_nodes.is_empty() {
        notes.push(format!(
            "Reconstructed graph had failed node(s): {}",
            failed_nodes.join("; ")
        ));
    }

    let Some(out_id) = graph.outputs.first() else {
        notes.push("Reconstructed graph produced no output node.".to_owned());
        return failed_report(
            orig_vol,
            orig_check.watertight,
            aag,
            recognition,
            graph,
            options,
            notes,
        );
    };
    let Some(reconstructed) = eval_report.output_solids.get(out_id) else {
        notes.push(format!("Output node `{out_id}` produced no solid."));
        return failed_report(
            orig_vol,
            orig_check.watertight,
            aag,
            recognition,
            graph,
            options,
            notes,
        );
    };

    let recon_check = kernel.check_solid(reconstructed);
    let recon_vol = recon_check.volume_mm3.max(0.0);
    let recon_bbox = if recon_check.bbox_mm.is_valid() {
        recon_check.bbox_mm
    } else {
        reconstructed.csg.bbox()
    };
    let bbox_max_delta_mm = bbox_max_delta(orig_bbox, recon_bbox);
    let centroid_delta_mm = centroid_delta(original, reconstructed);

    let (verification_method, symmetric_difference_mm3, relative_volume_error) =
        match kernel.measure_symmetric_difference(reconstructed, original) {
            Ok(sym) => (
                VerificationMethod::OcctSymmetricDifference,
                sym.symmetric_difference_mm3,
                sym.relative_error,
            ),
            Err(KernelError::BackendUnavailable | KernelError::UnsupportedMixedBoolean) => {
                let vol_diff = (recon_vol - orig_vol).abs();
                let vol_rel = if orig_vol > 1e-9 {
                    vol_diff / orig_vol
                } else {
                    1.0
                };
                let (sampled_sym_mm3, sampled_rel) = estimate_lattice_symmetric_difference(
                    original,
                    reconstructed,
                    orig_bbox.union(recon_bbox),
                    options.analytic_sampling_resolution,
                );
                let sym_mm3 = vol_diff.max(sampled_sym_mm3);
                let rel_err = vol_rel.max(sampled_rel);
                (
                    VerificationMethod::AnalyticVolumeAndSampling,
                    sym_mm3,
                    rel_err,
                )
            }
            Err(err) => {
                notes.push(format!("Exact symmetric difference unavailable: {err}"));
                let vol_diff = (recon_vol - orig_vol).abs();
                let vol_rel = if orig_vol > 1e-9 {
                    vol_diff / orig_vol
                } else {
                    1.0
                };
                let (sampled_sym_mm3, sampled_rel) = estimate_lattice_symmetric_difference(
                    original,
                    reconstructed,
                    orig_bbox.union(recon_bbox),
                    options.analytic_sampling_resolution,
                );
                (
                    VerificationMethod::AnalyticVolumeAndSampling,
                    vol_diff.max(sampled_sym_mm3),
                    vol_rel.max(sampled_rel),
                )
            }
        };

    let unrecognized_face_count = recognition.unrecognized_face_ids.len();
    let status = if !failed_nodes.is_empty()
        || !recon_check.watertight
        || !relative_volume_error.is_finite()
        || relative_volume_error >= options.max_relative_volume_error
        || bbox_max_delta_mm > options.max_bbox_delta_mm
    {
        if relative_volume_error >= options.max_relative_volume_error {
            notes.push(format!(
                "Relative volume error {:.6} exceeds threshold {:.6}.",
                relative_volume_error, options.max_relative_volume_error
            ));
        }
        if bbox_max_delta_mm > options.max_bbox_delta_mm {
            notes.push(format!(
                "Bounding-box delta {:.4} mm exceeds threshold {:.4} mm.",
                bbox_max_delta_mm, options.max_bbox_delta_mm
            ));
        }
        if !recon_check.watertight {
            notes.push("Reconstructed solid is not watertight.".to_owned());
        }
        DecompileStatus::Fail
    } else if !orig_check.watertight || unrecognized_face_count > 0 {
        if !orig_check.watertight {
            notes.push("Input solid is not watertight; volume closure cannot certify PASS.".to_owned());
        }
        if unrecognized_face_count > 0 {
            notes.push(format!(
                "{unrecognized_face_count} face(s) were not explained by recognized prismatic features."
            ));
        }
        DecompileStatus::Unknown
    } else {
        DecompileStatus::Pass
    };

    DecompileReport {
        status,
        verification_method,
        original_volume_mm3: orig_vol,
        reconstructed_volume_mm3: recon_vol,
        symmetric_difference_mm3,
        relative_volume_error,
        max_relative_volume_error: options.max_relative_volume_error,
        bbox_max_delta_mm,
        centroid_delta_mm,
        original_watertight: orig_check.watertight,
        reconstructed_watertight: recon_check.watertight,
        recognized_features: recognition.features(),
        dressup_hints: recognition.dressup_hints.clone(),
        total_face_count: aag.faces.len(),
        recognized_face_count: recognition.explained_face_ids.len(),
        unrecognized_face_count,
        parameter_count: graph.parameters.len(),
        node_count: graph.nodes.len(),
        notes,
    }
}

/// Save a decompiled [`CanonicalGraph`] and its `.kcl 2.0` view directly into a SQLite `.aicad` container.
pub fn save_decompiled_project(
    path: &Path,
    graph: &CanonicalGraph,
    kcl_source: &str,
    kernel_name: &str,
) -> Result<(), DecompileError> {
    let manifest = ContainerManifest::new(
        &graph.graph_id,
        KernelFingerprint::new(kernel_name, env!("CARGO_PKG_VERSION")),
    );
    let mut project = AicadProject::new(manifest, graph.clone());
    project.kcl_view = Some(kcl_source.to_owned());
    SqliteContainer::create(path, &project)?;
    Ok(())
}

fn failed_report(
    orig_vol: f64,
    orig_watertight: bool,
    aag: &AttributedAdjacencyGraph,
    recognition: &RecognitionResult,
    graph: &CanonicalGraph,
    options: &DecompileOptions,
    notes: Vec<String>,
) -> DecompileReport {
    DecompileReport {
        status: DecompileStatus::Fail,
        verification_method: VerificationMethod::EvaluationFailed,
        original_volume_mm3: orig_vol,
        reconstructed_volume_mm3: 0.0,
        symmetric_difference_mm3: orig_vol,
        relative_volume_error: 1.0,
        max_relative_volume_error: options.max_relative_volume_error,
        bbox_max_delta_mm: f64::INFINITY,
        centroid_delta_mm: f64::INFINITY,
        original_watertight: orig_watertight,
        reconstructed_watertight: false,
        recognized_features: recognition.features(),
        dressup_hints: recognition.dressup_hints.clone(),
        total_face_count: aag.faces.len(),
        recognized_face_count: recognition.explained_face_ids.len(),
        unrecognized_face_count: recognition.unrecognized_face_ids.len(),
        parameter_count: graph.parameters.len(),
        node_count: graph.nodes.len(),
        notes,
    }
}

fn estimate_lattice_symmetric_difference(
    original: &SolidShape,
    reconstructed: &SolidShape,
    union_bbox: BBox3,
    resolution: usize,
) -> (f64, f64) {
    let sz = union_bbox.size();
    let box_vol = sz[0] * sz[1] * sz[2];
    if !union_bbox.is_valid() || box_vol <= 1e-9 {
        return (0.0, 1.0);
    }

    // Cap sampling resolution for large discrete triangle meshes to remain fast.
    let mesh_tris = match &original.csg {
        CsgNode::Mesh { triangles, .. } => triangles.len(),
        CsgNode::Occt { triangles, .. } => triangles.len(),
        _ => 0,
    };
    let n = if mesh_tris > 2000 {
        resolution.clamp(8, 16)
    } else {
        resolution.clamp(12, 48)
    };

    let dx = sz[0] / n as f64;
    let dy = sz[1] / n as f64;
    let dz = sz[2] / n as f64;
    let total = (n * n * n) as f64;
    let mut only_orig = 0usize;
    let mut only_recon = 0usize;
    let mut both = 0usize;

    for ix in 0..n {
        let x = union_bbox.min[0] + (ix as f64 + 0.5) * dx;
        for iy in 0..n {
            let y = union_bbox.min[1] + (iy as f64 + 0.5) * dy;
            for iz in 0..n {
                let z = union_bbox.min[2] + (iz as f64 + 0.5) * dz;
                let p = [x, y, z];
                match (original.csg.contains(p), reconstructed.csg.contains(p)) {
                    (true, true) => both += 1,
                    (true, false) => only_orig += 1,
                    (false, true) => only_recon += 1,
                    (false, false) => {}
                }
            }
        }
    }

    let orig_cells = both + only_orig;
    let diff_cells = only_orig + only_recon;
    let sym_vol = box_vol * (diff_cells as f64 / total);
    let rel_err = if orig_cells == 0 {
        if diff_cells == 0 { 0.0 } else { 1.0 }
    } else {
        diff_cells as f64 / orig_cells as f64
    };
    (sym_vol, rel_err)
}

fn bbox_max_delta(a: BBox3, b: BBox3) -> f64 {
    if !a.is_valid() || !b.is_valid() {
        return f64::INFINITY;
    }
    let mut max_d = 0.0_f64;
    for k in 0..3 {
        max_d = max_d.max((a.min[k] - b.min[k]).abs());
        max_d = max_d.max((a.max[k] - b.max[k]).abs());
    }
    max_d
}

fn centroid_delta(a: &SolidShape, b: &SolidShape) -> f64 {
    let ca = surface_centroid(a);
    let cb = surface_centroid(b);
    let d = sub(ca, cb);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn surface_centroid(solid: &SolidShape) -> Vec3 {
    let mut area_sum = 0.0;
    let mut weighted = [0.0; 3];
    for f in &solid.faces {
        if f.area_mm2.is_finite() && f.area_mm2 > 0.0 && f.center_hint.iter().all(|v| v.is_finite())
        {
            area_sum += f.area_mm2;
            weighted = add(weighted, scale(f.center_hint, f.area_mm2));
        }
    }
    if area_sum > 1e-12 {
        scale(weighted, 1.0 / area_sum)
    } else {
        let bb = solid.csg.bbox();
        scale(add(bb.min, bb.max), 0.5)
    }
}

fn add_length_param(
    graph: &mut CanonicalGraph,
    name: &str,
    value_mm: f64,
) -> Result<(), DecompileError> {
    let clean = round_clean(value_mm.max(1e-4));
    graph.parameters.insert(
        name.to_owned(),
        ParameterDef {
            name: ParamId::from(name),
            ir_type: IrType::Length,
            value: IrValue::Quantity(Quantity::length(clean, LengthUnit::Mm)?),
            formula: None,
        },
    );
    Ok(())
}

fn add_int_param(graph: &mut CanonicalGraph, name: &str, value: i64) {
    graph.parameters.insert(
        name.to_owned(),
        ParameterDef {
            name: ParamId::from(name),
            ir_type: IrType::Int,
            value: IrValue::Int(value),
            formula: None,
        },
    );
}

fn insert_node(graph: &mut CanonicalGraph, node: IrNode) -> NodeId {
    let id = node.id.clone();
    graph.nodes.insert(id.0.clone(), node);
    id
}

fn round_clean(v: f64) -> f64 {
    let rounded = (v * 1e6).round() / 1e6;
    if rounded.abs() < 1e-12 { 0.0 } else { rounded }
}

fn round_vec3(v: Vec3) -> Vec3 {
    [round_clean(v[0]), round_clean(v[1]), round_clean(v[2])]
}

fn sanitize_graph_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "decompiled_part".to_owned()
    } else if trimmed.as_bytes()[0].is_ascii_digit() {
        format!("part_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

//! L3 manufacturability, Keep-Out, multi-body interference, and voxel FEA gate.
//!
//! Bridges evaluated `SolidShape` terminal bodies and `DesignContract` constraints
//! into `cad-gpu-compute`'s ROI-rebased LBVH / ray-beam DFM gate and H8 voxel
//! FEA solver, emitting deterministic `VerificationCheck` entries with
//! structured machine-readable repair hints.

use std::collections::BTreeMap;

use cad_api::{CanonicalGraph, DesignContract, NodeId, inspect_document_summary};
use cad_gpu_compute::{
    DfmAnalysisRequest, DfmConfig, DfmGateSummary, DfmRepairHint, DfmRepairKind, KeepOutZone,
    TriRef, run_dfm_gate,
};
use cad_ir::{
    DesignAssertionKind, DimInput, FeatureSpec, IrValue, TriVerdict, VerificationCheck,
    VerificationLayer, resolve_formula_values,
};
use cad_kernel_occt::SolidShape;
use serde_json::{Value, json};

const DEFAULT_MIN_WALL_MM: f64 = 0.05;
const DEFAULT_SAMPLES_PER_FACE: usize = 16;

#[derive(Debug, Default)]
struct ContractDfmRequirements {
    min_wall_thickness_mm: f64,
    min_draft_angle_deg: f64,
    pull_dir: [f64; 3],
    tool_axes: Vec<[f64; 3]>,
    keep_out_zones: Vec<KeepOutZone>,
    keep_out_parse_error: Option<String>,
    min_clearance_mm: Option<f64>,
    manufacturing_method: Option<String>,
}

/// Evaluates all L3 manufacturability, Keep-Out, multi-body interference, and
/// H8 voxel FEA structural checks for the terminal solids of `graph`.
pub(crate) fn evaluate_l3_checks(
    graph: &CanonicalGraph,
    terminal_solids: &[(&NodeId, &SolidShape)],
    fea_blob_declared: bool,
) -> Vec<VerificationCheck> {
    let summary = inspect_document_summary(graph);
    let design_contract = summary.design_contract.as_ref();
    let req = extract_requirements(graph, design_contract);
    let mut checks = Vec::new();

    if terminal_solids.is_empty() {
        if let Some(fea_check) =
            crate::l3_fea::evaluate_design_contract_fea(design_contract, &[], fea_blob_declared)
        {
            checks.push(fea_check);
        }
        checks.push(
            VerificationCheck::new(
                "L3_DFM",
                VerificationLayer::L3ManufacturabilityPhysics,
                TriVerdict::Unknown,
                "no evaluated solid for L3 ray-beam DFM",
            )
            .with_evidence("min_wall_mm", req.min_wall_thickness_mm),
        );
        return checks;
    }

    let (triangles, face_labels, instance_labels) = build_triangle_scene(terminal_solids);
    if triangles.is_empty() {
        if let Some(fea_check) =
            crate::l3_fea::evaluate_design_contract_fea(design_contract, &[], fea_blob_declared)
        {
            checks.push(fea_check);
        }
        checks.push(
            VerificationCheck::new(
                "L3_DFM",
                VerificationLayer::L3ManufacturabilityPhysics,
                TriVerdict::Unknown,
                "evaluated solids have no non-degenerate triangles for L3 ray-beam DFM",
            )
            .with_evidence("min_wall_mm", req.min_wall_thickness_mm),
        );
        return checks;
    }

    if let Some(fea_check) =
        crate::l3_fea::evaluate_design_contract_fea(design_contract, &triangles, fea_blob_declared)
    {
        checks.push(fea_check);
    }

    let config = DfmConfig {
        min_wall_thickness_mm: req.min_wall_thickness_mm,
        min_draft_angle_deg: req.min_draft_angle_deg,
        pull_dir: req.pull_dir,
        tool_axes: req.tool_axes.clone(),
        samples_per_face: DEFAULT_SAMPLES_PER_FACE,
    };
    let mut request = DfmAnalysisRequest::new(triangles, config)
        .with_keep_out_zones(req.keep_out_zones.clone())
        .with_face_labels(face_labels)
        .with_instance_labels(instance_labels);
    if let Some(clearance) = req.min_clearance_mm {
        request = request.with_min_clearance_mm(clearance);
    } else {
        request.check_multi_body_interference = terminal_solids.len() >= 2;
    }

    let summary = run_dfm_gate(&request);
    checks.push(build_dfm_check(&summary, &req));

    if let Some(ref parse_err) = req.keep_out_parse_error {
        checks.push(
            VerificationCheck::new(
                "L3_KEEPOUT",
                VerificationLayer::L3ManufacturabilityPhysics,
                TriVerdict::Unknown,
                format!("declared keep_out_zones could not be parsed: {parse_err}"),
            )
            .with_evidence("parse_error", parse_err.clone()),
        );
    } else if !req.keep_out_zones.is_empty() {
        checks.push(build_keepout_check(&summary, req.keep_out_zones.len()));
    }

    if terminal_solids.len() >= 2 || req.min_clearance_mm.is_some() {
        checks.push(build_interference_check(
            &summary,
            terminal_solids.len(),
            req.min_clearance_mm.unwrap_or(0.0),
        ));
    }

    checks
}

/// Evaluates an explicit `L2_MIN_WALL` assertion using the ROI-rebased ray-beam
/// DFM gate and returns `(status, message, summary)` for structured evidence.
pub(crate) fn evaluate_min_wall_check(
    terminal_solids: &[(&NodeId, &SolidShape)],
    min_wall_mm: f64,
) -> (TriVerdict, String, Option<DfmGateSummary>) {
    if terminal_solids.is_empty() {
        return (
            TriVerdict::Unknown,
            "no evaluated solid for DFM".to_owned(),
            None,
        );
    }
    let (triangles, face_labels, instance_labels) = build_triangle_scene(terminal_solids);
    if triangles.is_empty() {
        return (
            TriVerdict::Unknown,
            "evaluated solids have no triangles for DFM".to_owned(),
            None,
        );
    }
    let config = DfmConfig {
        min_wall_thickness_mm: min_wall_mm,
        min_draft_angle_deg: 0.0,
        pull_dir: [0.0, 0.0, 0.0],
        tool_axes: Vec::new(),
        samples_per_face: DEFAULT_SAMPLES_PER_FACE,
    };
    let mut request = DfmAnalysisRequest::new(triangles, config)
        .with_face_labels(face_labels)
        .with_instance_labels(instance_labels);
    request.check_multi_body_interference = false;

    let summary = run_dfm_gate(&request);
    if summary.thin_wall_face_count > 0 {
        let measured = summary
            .min_measured_wall_mm
            .map(|v| format!(" (min measured {v:.4} mm)"))
            .unwrap_or_default();
        (
            TriVerdict::Fail,
            format!(
                "ray-beam DFM found {} thin-wall face violation(s) below {min_wall_mm} mm{measured}",
                summary.thin_wall_face_count
            ),
            Some(summary),
        )
    } else if summary.face_count > 0 {
        (
            TriVerdict::Pass,
            format!("ray-beam DFM is clear at {min_wall_mm} mm"),
            Some(summary),
        )
    } else {
        (
            TriVerdict::Unknown,
            "evaluated solids have no triangles for DFM".to_owned(),
            Some(summary),
        )
    }
}

fn build_dfm_check(
    summary: &DfmGateSummary,
    req: &ContractDfmRequirements,
) -> VerificationCheck {
    let dfm_hints: Vec<&DfmRepairHint> = summary
        .repair_hints
        .iter()
        .filter(|hint| {
            matches!(
                hint.kind,
                DfmRepairKind::IncreaseWallThickness
                    | DfmRepairKind::IncreaseDraftAngle
                    | DfmRepairKind::ImproveToolAccessibility
            )
        })
        .collect();

    let (status, message) = if summary.face_count == 0 {
        (
            TriVerdict::Unknown,
            "ray-beam DFM had no non-degenerate faces to evaluate".to_owned(),
        )
    } else if !summary.dfm_report.is_clear() {
        let mut reasons = Vec::new();
        if summary.thin_wall_face_count > 0 {
            let min_str = summary
                .min_measured_wall_mm
                .map(|v| format!(" (min {v:.4} mm)"))
                .unwrap_or_default();
            reasons.push(format!(
                "{} thin-wall violation(s) < {:.4} mm{min_str}",
                summary.thin_wall_face_count, req.min_wall_thickness_mm
            ));
        }
        if summary.draft_violation_face_count > 0 {
            reasons.push(format!(
                "{} draft-angle violation(s) < {:.2} deg",
                summary.draft_violation_face_count, req.min_draft_angle_deg
            ));
        }
        if summary.inaccessible_face_count > 0 {
            reasons.push(format!(
                "{} tool-accessibility violation(s)",
                summary.inaccessible_face_count
            ));
        }
        (
            TriVerdict::Fail,
            format!("ray-beam DFM failed: {}", reasons.join("; ")),
        )
    } else {
        (
            TriVerdict::Pass,
            format!(
                "ray-beam DFM passed across {} face(s) at min_wall={:.4} mm (backend={})",
                summary.face_count, req.min_wall_thickness_mm, summary.backend
            ),
        )
    };

    let mut check = VerificationCheck::new(
        "L3_DFM",
        VerificationLayer::L3ManufacturabilityPhysics,
        status,
        message,
    )
    .with_evidence("backend", summary.backend.clone())
    .with_evidence("roi_origin_mm", json!(summary.roi_origin_mm))
    .with_evidence("triangle_count", summary.triangle_count as u64)
    .with_evidence("face_count", summary.face_count as u64)
    .with_evidence("min_wall_mm", req.min_wall_thickness_mm)
    .with_evidence("thin_wall_face_count", summary.thin_wall_face_count as u64)
    .with_evidence(
        "draft_violation_face_count",
        summary.draft_violation_face_count as u64,
    )
    .with_evidence(
        "inaccessible_face_count",
        summary.inaccessible_face_count as u64,
    );

    if let Some(min_wall) = summary.min_measured_wall_mm {
        check = check.with_evidence("min_measured_wall_mm", min_wall);
    }
    if let Some(min_draft) = summary.min_measured_draft_deg {
        check = check.with_evidence("min_measured_draft_deg", min_draft);
    }
    if let Some(ref method) = req.manufacturing_method {
        check = check.with_evidence("manufacturing_method", method.clone());
    }
    if !dfm_hints.is_empty() {
        check = check.with_evidence("repair_hints", json!(dfm_hints));
    }
    check
}

fn build_keepout_check(summary: &DfmGateSummary, checked_zone_count: usize) -> VerificationCheck {
    let keepout_hints: Vec<&DfmRepairHint> = summary
        .repair_hints
        .iter()
        .filter(|hint| hint.kind == DfmRepairKind::ClearKeepOutZone)
        .collect();

    if summary.keepout_report.is_clear() {
        VerificationCheck::new(
            "L3_KEEPOUT",
            VerificationLayer::L3ManufacturabilityPhysics,
            TriVerdict::Pass,
            format!("all {checked_zone_count} keep-out zone(s) are clear (exact full-triangle LBVH)"),
        )
        .with_evidence("checked_zone_count", checked_zone_count as u64)
        .with_evidence("violation_count", 0u64)
        .with_evidence("roi_origin_mm", json!(summary.roi_origin_mm))
    } else {
        let zones: Vec<&str> = summary
            .keepout_report
            .violations
            .iter()
            .map(|v| v.zone_id.as_str())
            .collect();
        VerificationCheck::new(
            "L3_KEEPOUT",
            VerificationLayer::L3ManufacturabilityPhysics,
            TriVerdict::Fail,
            format!(
                "keep-out intrusion detected in {} violation(s): [{}]",
                summary.keepout_violation_count,
                zones.join(", ")
            ),
        )
        .with_evidence("checked_zone_count", checked_zone_count as u64)
        .with_evidence("violation_count", summary.keepout_violation_count as u64)
        .with_evidence("violations", json!(summary.keepout_report.violations))
        .with_evidence("repair_hints", json!(keepout_hints))
    }
}

fn build_interference_check(
    summary: &DfmGateSummary,
    instance_count: usize,
    min_clearance_mm: f64,
) -> VerificationCheck {
    let interference_hints: Vec<&DfmRepairHint> = summary
        .repair_hints
        .iter()
        .filter(|hint| hint.kind == DfmRepairKind::ResolveBodyInterference)
        .collect();

    if summary.interference_report.is_clear() {
        VerificationCheck::new(
            "L3_INTERFERENCE",
            VerificationLayer::L3ManufacturabilityPhysics,
            TriVerdict::Pass,
            format!(
                "multi-body LBVH interference clear across {instance_count} body(ies) (min_clearance={min_clearance_mm:.4} mm)"
            ),
        )
        .with_evidence("instance_count", instance_count as u64)
        .with_evidence("min_clearance_mm", min_clearance_mm)
        .with_evidence("pair_count", 0u64)
    } else {
        let penetrating = summary
            .interference_report
            .pairs
            .iter()
            .filter(|pair| pair.penetrating)
            .count();
        VerificationCheck::new(
            "L3_INTERFERENCE",
            VerificationLayer::L3ManufacturabilityPhysics,
            TriVerdict::Fail,
            format!(
                "multi-body LBVH interference detected {} pair(s) ({penetrating} penetrating) below clearance {min_clearance_mm:.4} mm",
                summary.interference_pair_count
            ),
        )
        .with_evidence("instance_count", instance_count as u64)
        .with_evidence("min_clearance_mm", min_clearance_mm)
        .with_evidence("pair_count", summary.interference_pair_count as u64)
        .with_evidence("pairs", json!(summary.interference_report.pairs))
        .with_evidence("repair_hints", json!(interference_hints))
    }
}

pub(crate) fn build_triangle_scene(
    terminal_solids: &[(&NodeId, &SolidShape)],
) -> (Vec<TriRef>, BTreeMap<u32, String>, BTreeMap<u32, String>) {
    let mut triangles = Vec::new();
    let mut face_labels = BTreeMap::new();
    let mut instance_labels = BTreeMap::new();
    let mut next_slot = 0u32;

    for (instance_idx, (node_id, solid)) in terminal_solids.iter().enumerate() {
        let instance_id = u32::try_from(instance_idx).unwrap_or(u32::MAX);
        instance_labels.insert(instance_id, node_id.0.clone());

        for (face_idx, face) in solid.faces.iter().enumerate() {
            let vertices: Vec<[f64; 3]> = face
                .vertices
                .chunks_exact(3)
                .map(|point| [point[0], point[1], point[2]])
                .collect();
            let mut face_tris = Vec::new();
            for tri in face.indices.chunks_exact(3) {
                let Some(a) = vertices.get(tri[0] as usize).copied() else {
                    continue;
                };
                let Some(b) = vertices.get(tri[1] as usize).copied() else {
                    continue;
                };
                let Some(c) = vertices.get(tri[2] as usize).copied() else {
                    continue;
                };
                if triangle_area(a, b, c) < 1.0e-8 {
                    continue;
                }
                face_tris.push(TriRef {
                    instance_id,
                    face_slot_id: next_slot,
                    a,
                    b,
                    c,
                });
            }
            if !face_tris.is_empty() {
                let label = if let Some(tag) = face.provenance_tags.first() {
                    format!("{}:face_{face_idx}({tag})", node_id.0)
                } else {
                    format!("{}:face_{face_idx}", node_id.0)
                };
                face_labels.insert(next_slot, label);
                triangles.extend(face_tris);
                next_slot = next_slot.saturating_add(1);
            }
        }
    }

    (triangles, face_labels, instance_labels)
}

fn triangle_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt()
}

fn extract_requirements(
    graph: &CanonicalGraph,
    design_contract: Option<&DesignContract>,
) -> ContractDfmRequirements {
    let mut req = ContractDfmRequirements {
        min_wall_thickness_mm: DEFAULT_MIN_WALL_MM,
        ..ContractDfmRequirements::default()
    };

    let mut explicit_min_wall: Option<f64> = declared_assertion_min_wall(graph);
    if let Some(contract) = design_contract {
        apply_design_contract(&mut req, &mut explicit_min_wall, contract);
    }

    if let Some(min_wall) = explicit_min_wall {
        req.min_wall_thickness_mm = min_wall;
    }

    if req.min_draft_angle_deg > 0.0 && req.pull_dir == [0.0, 0.0, 0.0] {
        req.pull_dir = [0.0, 0.0, 1.0];
    } else if req.pull_dir != [0.0, 0.0, 0.0] && req.min_draft_angle_deg == 0.0 {
        if req.manufacturing_method.as_deref().is_some_and(|m| {
            let upper = m.to_ascii_uppercase();
            upper.contains("MOLD") || upper.contains("CAST")
        }) {
            req.min_draft_angle_deg = 1.0;
        }
    }

    req
}

fn apply_design_contract(
    req: &mut ContractDfmRequirements,
    explicit_min_wall: &mut Option<f64>,
    contract: &DesignContract,
) {
    req.manufacturing_method = contract.manufacturing_method.clone();
    let attrs = &contract.attributes;

    // 1. Optional nested `dfm_constraints` JSON object (matches schemas/design-contract.schema.json).
    if let Some(raw_dfm) = attrs.get("dfm_constraints")
        && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw_dfm)
    {
        if let Some(mm) = map.get("min_wall_thickness_mm").and_then(Value::as_f64)
            && mm.is_finite()
            && mm > 0.0
        {
            *explicit_min_wall = Some(explicit_min_wall.map_or(mm, |cur| cur.max(mm)));
        }
        if let Some(deg) = map.get("min_draft_angle_deg").and_then(Value::as_f64)
            && deg.is_finite()
            && deg >= 0.0
        {
            req.min_draft_angle_deg = req.min_draft_angle_deg.max(deg);
        }
        if let Some(pull) = map.get("pull_dir").and_then(parse_axis_value) {
            req.pull_dir = pull;
        }
        if let Some(axes) = map
            .get("accessible_axes")
            .or_else(|| map.get("tool_axes"))
            .and_then(parse_axes_value)
        {
            req.tool_axes = axes;
        }
        if let Some(clearance) = map.get("min_clearance_mm").and_then(Value::as_f64)
            && clearance.is_finite()
            && clearance >= 0.0
        {
            req.min_clearance_mm =
                Some(req.min_clearance_mm.map_or(clearance, |cur| cur.max(clearance)));
        }
    }

    // 2. Direct flat attributes on `DesignContract.attributes`.
    if let Some(mm) = parse_positive_f64_attr(attrs, "min_wall_thickness_mm")
        .or_else(|| parse_positive_f64_attr(attrs, "min_wall_mm"))
    {
        *explicit_min_wall = Some(explicit_min_wall.map_or(mm, |cur| cur.max(mm)));
    }
    if let Some(deg) = parse_nonneg_f64_attr(attrs, "min_draft_angle_deg") {
        req.min_draft_angle_deg = req.min_draft_angle_deg.max(deg);
    }
    if let Some(raw_pull) = attrs.get("pull_dir")
        && let Some(axis) = parse_axis_str_or_json(raw_pull)
    {
        req.pull_dir = axis;
    }
    if let Some(raw_axes) = attrs.get("accessible_axes").or_else(|| attrs.get("tool_axes"))
        && let Some(axes) = parse_axes_str_or_json(raw_axes)
    {
        req.tool_axes = axes;
    }
    if let Some(clearance) = parse_nonneg_f64_attr(attrs, "min_clearance_mm") {
        req.min_clearance_mm =
            Some(req.min_clearance_mm.map_or(clearance, |cur| cur.max(clearance)));
    }

    // 3. Keep-Out zones JSON array.
    if let Some(raw_zones) = attrs.get("keep_out_zones") {
        match parse_keep_out_zones(raw_zones) {
            Ok(zones) => req.keep_out_zones.extend(zones),
            Err(err) => req.keep_out_parse_error = Some(err),
        }
    }
}

fn declared_assertion_min_wall(graph: &CanonicalGraph) -> Option<f64> {
    let mut strict = None;
    for node in graph.nodes.values() {
        let Ok(FeatureSpec::DesignAssertion {
            kind: DesignAssertionKind::MinWallThickness { min },
            ..
        }) = node.try_feature_spec()
        else {
            continue;
        };
        if let Some(mm) = length_mm(graph, &min)
            && mm.is_finite()
            && mm > 0.0
        {
            strict = Some(strict.map_or(mm, |current: f64| current.max(mm)));
        }
    }
    strict
}

fn length_mm(graph: &CanonicalGraph, dim: &DimInput) -> Option<f64> {
    match dim {
        DimInput::Literal(quantity) => Some(quantity.canonical_value()),
        DimInput::Param { param_id } => {
            if let Ok((_, values)) = resolve_formula_values(graph)
                && let Some(IrValue::Quantity(quantity)) = values.get(param_id)
            {
                return Some(quantity.canonical_value());
            }
            match graph.parameters.get(&param_id.0).map(|def| &def.value) {
                Some(IrValue::Quantity(quantity)) => Some(quantity.canonical_value()),
                Some(IrValue::Scalar(value)) if value.is_finite() => Some(*value),
                _ => None,
            }
        }
        DimInput::NodeOutput { .. } => None,
    }
}

fn parse_positive_f64_attr(attrs: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    let value: f64 = attrs.get(key)?.trim().parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

fn parse_nonneg_f64_attr(attrs: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    let value: f64 = attrs.get(key)?.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn parse_axis_token(raw: &str) -> Option<[f64; 3]> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "+X" | "X" | "X_AXIS" => Some([1.0, 0.0, 0.0]),
        "-X" => Some([-1.0, 0.0, 0.0]),
        "+Y" | "Y" | "Y_AXIS" => Some([0.0, 1.0, 0.0]),
        "-Y" => Some([0.0, -1.0, 0.0]),
        "+Z" | "Z" | "Z_AXIS" => Some([0.0, 0.0, 1.0]),
        "-Z" => Some([0.0, 0.0, -1.0]),
        _ => None,
    }
}

fn parse_vec3_value(value: &Value) -> Option<[f64; 3]> {
    let arr = value.as_array()?;
    if arr.len() != 3 {
        return None;
    }
    let x = arr[0].as_f64()?;
    let y = arr[1].as_f64()?;
    let z = arr[2].as_f64()?;
    (x.is_finite() && y.is_finite() && z.is_finite()).then_some([x, y, z])
}

fn parse_axis_value(value: &Value) -> Option<[f64; 3]> {
    if let Some(s) = value.as_str() {
        parse_axis_token(s)
    } else {
        parse_vec3_value(value)
    }
}

fn parse_axis_str_or_json(raw: &str) -> Option<[f64; 3]> {
    if let Some(axis) = parse_axis_token(raw) {
        return Some(axis);
    }
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| parse_axis_value(&v))
}

fn parse_axes_value(value: &Value) -> Option<Vec<[f64; 3]>> {
    let arr = value.as_array()?;
    let mut axes = Vec::with_capacity(arr.len());
    for item in arr {
        axes.push(parse_axis_value(item)?);
    }
    Some(axes)
}

fn parse_axes_str_or_json(raw: &str) -> Option<Vec<[f64; 3]>> {
    if let Ok(value) = serde_json::from_str::<Value>(raw)
        && let Some(axes) = parse_axes_value(&value)
    {
        return Some(axes);
    }
    let mut axes = Vec::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        axes.push(parse_axis_token(trimmed)?);
    }
    (!axes.is_empty()).then_some(axes)
}

fn parse_keep_out_zones(raw: &str) -> Result<Vec<KeepOutZone>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|err| format!("invalid JSON: {err}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "keep_out_zones must be a JSON array".to_owned())?;
    let mut zones = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        if let Ok(direct) = serde_json::from_value::<KeepOutZone>(item.clone()) {
            zones.push(direct);
            continue;
        }
        zones.push(parse_schema_keep_out_zone(item, idx)?);
    }
    Ok(zones)
}

fn parse_schema_keep_out_zone(item: &Value, index: usize) -> Result<KeepOutZone, String> {
    let obj = item
        .as_object()
        .ok_or_else(|| format!("keep_out_zones[{index}] must be an object"))?;
    let zone_id = obj
        .get("zone_id")
        .or_else(|| obj.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("keepout_{index}"));
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("keep_out_zones[{index}] missing `kind`"))?
        .to_ascii_lowercase();
    let description = obj
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut zone = match kind.as_str() {
        "box" | "aabb" => {
            let (min_mm, max_mm) = if let (Some(min_v), Some(max_v)) = (
                obj.get("min_mm").or_else(|| obj.get("min")).and_then(parse_vec3_value),
                obj.get("max_mm").or_else(|| obj.get("max")).and_then(parse_vec3_value),
            ) {
                (min_v, max_v)
            } else if let Some(bbox) = obj.get("bbox_mm").and_then(Value::as_object) {
                let min_v = bbox
                    .get("min")
                    .and_then(parse_vec3_value)
                    .ok_or_else(|| format!("keep_out_zones[{index}] invalid bbox_mm.min"))?;
                let max_v = bbox
                    .get("max")
                    .and_then(parse_vec3_value)
                    .ok_or_else(|| format!("keep_out_zones[{index}] invalid bbox_mm.max"))?;
                (min_v, max_v)
            } else {
                let origin = obj
                    .get("origin_mm")
                    .and_then(parse_vec3_value)
                    .ok_or_else(|| format!("keep_out_zones[{index}] box missing origin_mm or min_mm/max_mm"))?;
                let size = obj
                    .get("size_mm")
                    .and_then(parse_vec3_value)
                    .ok_or_else(|| format!("keep_out_zones[{index}] box missing size_mm"))?;
                (
                    origin,
                    [origin[0] + size[0], origin[1] + size[1], origin[2] + size[2]],
                )
            };
            KeepOutZone::box_mm(zone_id, min_mm, max_mm)
        }
        "sphere" => {
            let center = obj
                .get("center_mm")
                .or_else(|| obj.get("origin_mm"))
                .and_then(parse_vec3_value)
                .ok_or_else(|| format!("keep_out_zones[{index}] sphere missing origin_mm/center_mm"))?;
            let radius = obj
                .get("radius_mm")
                .and_then(Value::as_f64)
                .or_else(|| obj.get("diameter_mm").and_then(Value::as_f64).map(|d| d * 0.5))
                .ok_or_else(|| format!("keep_out_zones[{index}] sphere missing radius_mm/diameter_mm"))?;
            KeepOutZone::sphere_mm(zone_id, center, radius)
        }
        "cylinder" => {
            let origin = obj
                .get("origin_mm")
                .and_then(parse_vec3_value)
                .ok_or_else(|| format!("keep_out_zones[{index}] cylinder missing origin_mm"))?;
            let axis = obj
                .get("axis")
                .and_then(parse_axis_value)
                .unwrap_or([0.0, 0.0, 1.0]);
            let radius = obj
                .get("radius_mm")
                .and_then(Value::as_f64)
                .or_else(|| obj.get("diameter_mm").and_then(Value::as_f64).map(|d| d * 0.5))
                .ok_or_else(|| format!("keep_out_zones[{index}] cylinder missing radius_mm/diameter_mm"))?;
            let height = obj
                .get("height_mm")
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("keep_out_zones[{index}] cylinder missing height_mm"))?;
            KeepOutZone::cylinder_mm(zone_id, origin, axis, radius, height)
        }
        other => {
            return Err(format!(
                "keep_out_zones[{index}] has unsupported kind `{other}`"
            ));
        }
    };

    if let Some(desc) = description {
        zone = zone.with_description(desc);
    }
    Ok(zone)
}

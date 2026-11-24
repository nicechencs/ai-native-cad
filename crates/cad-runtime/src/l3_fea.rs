//! L3 structural FEA verification gate backed by `cad-gpu-compute`'s matrix-free
//! 3D H8 voxel linear-elastic static solver (H2-E07).
//!
//! Bridges evaluated `SceneMeshPayload` / `TriRef` geometry and `DesignContract`
//! structural boundary conditions into `solve_mesh_voxel_fea_auto`, upgrading
//! `L3_FEA` from `Unknown` to deterministic `Pass` or `Fail` when complete
//! material, support, and load specifications are present, while staying
//! honestly `Unknown` on missing boundary conditions, rigid-body instability,
//! or non-convergence.

use std::collections::BTreeMap;

use cad_api::DesignContract;
use cad_gpu_compute::{
    FeaGateVerdict, FeaMaterial, FeaRegion, FixedConstraint, ForceLoad, TriRef, VoxelFeaConfig,
    VoxelFeaReport, solve_mesh_voxel_fea_auto,
};
use cad_ir::{TriVerdict, VerificationCheck, VerificationLayer};
use cad_scene_mesh::SceneMeshPayload;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_MIN_SAFETY_FACTOR: f64 = 1.5;
const DEFAULT_MAX_VOXELS_PER_AXIS: usize = 16;
const MIN_VOXELS_PER_AXIS: usize = 4;
const MAX_VOXELS_PER_AXIS: usize = 48;
const DEFAULT_SOLVER_TOLERANCE: f64 = 1.0e-6;
const DEFAULT_SOLVER_MAX_ITERS: usize = 2000;
const SOLVER_NAME: &str = "h8_voxel_pcg";

const fn default_min_safety_factor() -> f64 {
    DEFAULT_MIN_SAFETY_FACTOR
}

const fn default_max_voxels_per_axis() -> usize {
    DEFAULT_MAX_VOXELS_PER_AXIS
}

const fn default_solver_tolerance() -> f64 {
    DEFAULT_SOLVER_TOLERANCE
}

const fn default_solver_max_iters() -> usize {
    DEFAULT_SOLVER_MAX_ITERS
}

/// Structural load, boundary condition, material, and acceptance criteria for
/// the L3 voxel FEA verification gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuralFeaContract {
    /// Material id or alias resolved through `schemas/materials.json` when
    /// `material` is not explicitly provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
    /// Explicit linear-elastic material override (`E` in MPa, `nu`, yield in MPa).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<FeaMaterial>,
    /// Dirichlet zero-displacement support regions.
    #[serde(default)]
    pub constraints: Vec<FixedConstraint>,
    /// Static nodal force loads.
    #[serde(default)]
    pub loads: Vec<ForceLoad>,
    /// Minimum required safety factor (`yield_strength_mpa / max_von_mises_mpa`).
    #[serde(default = "default_min_safety_factor")]
    pub min_safety_factor: f64,
    /// Optional explicit upper bound on peak Von Mises stress in MPa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_allowable_von_mises_mpa: Option<f64>,
    /// Voxel count along the longest bounding-box axis (clamped to `4..=48`).
    #[serde(default = "default_max_voxels_per_axis")]
    pub max_voxels_per_axis: usize,
    /// Relative residual convergence tolerance for Jacobi-PCG.
    #[serde(default = "default_solver_tolerance")]
    pub tolerance: f64,
    /// Maximum PCG iterations.
    #[serde(default = "default_solver_max_iters")]
    pub max_iters: usize,
}

impl Default for StructuralFeaContract {
    fn default() -> Self {
        Self {
            material_id: None,
            material: None,
            constraints: Vec::new(),
            loads: Vec::new(),
            min_safety_factor: DEFAULT_MIN_SAFETY_FACTOR,
            max_allowable_von_mises_mpa: None,
            max_voxels_per_axis: DEFAULT_MAX_VOXELS_PER_AXIS,
            tolerance: DEFAULT_SOLVER_TOLERANCE,
            max_iters: DEFAULT_SOLVER_MAX_ITERS,
        }
    }
}

impl StructuralFeaContract {
    /// Creates a structural FEA contract with the given supports and loads.
    #[must_use]
    pub fn new(constraints: Vec<FixedConstraint>, loads: Vec<ForceLoad>) -> Self {
        Self {
            constraints,
            loads,
            ..Self::default()
        }
    }

    /// Sets the catalog material id (looked up in `schemas/materials.json`).
    #[must_use]
    pub fn with_material_id(mut self, material_id: impl Into<String>) -> Self {
        self.material_id = Some(material_id.into());
        self
    }

    /// Sets an explicit isotropic linear-elastic material override.
    #[must_use]
    pub fn with_material(mut self, material: FeaMaterial) -> Self {
        self.material = Some(material);
        self
    }

    /// Sets the minimum required safety factor for `Pass`.
    #[must_use]
    pub fn with_min_safety_factor(mut self, min_safety_factor: f64) -> Self {
        self.min_safety_factor = min_safety_factor;
        self
    }

    /// Sets an explicit allowable Von Mises stress limit in MPa.
    #[must_use]
    pub fn with_max_allowable_von_mises_mpa(mut self, max_allowable_von_mises_mpa: f64) -> Self {
        self.max_allowable_von_mises_mpa = Some(max_allowable_von_mises_mpa);
        self
    }

    /// Sets the voxel resolution along the longest bounding-box axis.
    #[must_use]
    pub fn with_max_voxels_per_axis(mut self, max_voxels_per_axis: usize) -> Self {
        self.max_voxels_per_axis = max_voxels_per_axis;
        self
    }

    /// Sets the PCG relative residual convergence tolerance.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: f64) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Sets the maximum number of PCG iterations.
    #[must_use]
    pub fn with_max_iters(mut self, max_iters: usize) -> Self {
        self.max_iters = max_iters;
        self
    }
}

/// Machine-readable repair or setup guidance emitted by the L3 FEA gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeaRepairHint {
    pub code: String,
    pub recommended_direction: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_stress_point_mm: Option<[f64; 3]>,
}

/// Lightweight structural FEA summary payload suitable for UI, CLI, MCP, and
/// agent consumption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeaGateSummaryPayload {
    pub verdict: TriVerdict,
    pub solver: String,
    pub solver_mounted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youngs_modulus_mpa: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poissons_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_strength_mpa: Option<f64>,
    pub required_safety_factor: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowable_von_mises_mpa: Option<f64>,
    pub converged: bool,
    pub iterations: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_residual: Option<f64>,
    pub active_voxel_count: usize,
    pub active_dof_count: usize,
    pub max_displacement_mm: f64,
    pub max_von_mises_mpa: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_stress_point_mm: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repair_hints: Vec<FeaRepairHint>,
}

/// Complete output of the L3 structural FEA gate: a `VerificationCheck` for
/// the five-layer report, a lightweight summary payload, and the grid-ordered
/// per-voxel Von Mises stress field (`MPa`) for downstream visualization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L3FeaEvaluation {
    pub check: VerificationCheck,
    pub summary: FeaGateSummaryPayload,
    pub voxel_von_mises_mpa: Vec<f32>,
}

/// Resolves an isotropic linear-elastic material from either an explicit
/// `FeaMaterial` override or the single-source-of-truth `schemas/materials.json`
/// catalog (or an inline JSON material specification).
pub fn resolve_fea_material(
    material_id: Option<&str>,
    explicit: Option<FeaMaterial>,
) -> Result<(String, FeaMaterial), String> {
    if let Some(mat) = explicit {
        if !mat.is_valid() {
            return Err(
                "invalid explicit FEA material (requires finite E > 0, 0 <= nu <= 0.49, yield > 0)"
                    .to_owned(),
            );
        }
        let label = material_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("custom")
            .to_owned();
        return Ok((label, mat));
    }

    let Some(raw_id) = material_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(
            "no engineering material specified (set DesignContract.material to a schemas/materials.json id such as Al-6061-T6, SUS304, Ti-6Al-4V, or provide explicit FeaMaterial)"
                .to_owned(),
        );
    };

    if let Ok(spec) = cad_stdlib::lookup_material(raw_id) {
        let mat = FeaMaterial {
            youngs_modulus_mpa: spec.youngs_modulus_gpa * 1000.0,
            poissons_ratio: spec.poissons_ratio,
            yield_strength_mpa: spec.yield_strength_mpa,
        };
        if !mat.is_valid() {
            return Err(format!(
                "catalog material `{}` has non-physical elastic constants",
                spec.id
            ));
        }
        return Ok((spec.id, mat));
    }

    if raw_id.starts_with('{')
        && let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(raw_id)
    {
        return parse_inline_material_object(&obj);
    }

    Err(format!(
        "unknown material `{raw_id}` not found in schemas/materials.json"
    ))
}

/// Evaluates the L3 structural FEA gate on an evaluated `SceneMeshPayload`.
#[must_use]
pub fn evaluate_l3_fea(
    scene_mesh: &SceneMeshPayload,
    contract: &StructuralFeaContract,
) -> L3FeaEvaluation {
    let triangles = triangles_from_scene_mesh(scene_mesh);
    evaluate_l3_fea_triangles(&triangles, contract)
}

/// Evaluates the L3 structural FEA gate on a triangle soup in world millimeters.
#[must_use]
pub fn evaluate_l3_fea_triangles(
    triangles: &[TriRef],
    contract: &StructuralFeaContract,
) -> L3FeaEvaluation {
    if triangles.is_empty() {
        return build_unknown_evaluation(
            contract,
            None,
            None,
            "no non-degenerate mesh triangles available for L3 voxel FEA",
            vec![FeaRepairHint {
                code: "FEA_EMPTY_GEOMETRY".to_owned(),
                recommended_direction: "generate_watertight_solid".to_owned(),
                summary: "Ensure the part evaluates to a non-empty watertight solid before running L3 FEA.".to_owned(),
                measured_value: None,
                required_value: None,
                peak_stress_point_mm: None,
            }],
        );
    }

    if !contract.min_safety_factor.is_finite() || contract.min_safety_factor <= 0.0 {
        return build_unknown_evaluation(
            contract,
            None,
            None,
            "invalid min_safety_factor (must be finite and > 0)",
            vec![FeaRepairHint {
                code: "FEA_INVALID_CONTRACT".to_owned(),
                recommended_direction: "set_positive_safety_factor".to_owned(),
                summary: "Provide a positive finite min_safety_factor (for example 1.5).".to_owned(),
                measured_value: None,
                required_value: Some(DEFAULT_MIN_SAFETY_FACTOR),
                peak_stress_point_mm: None,
            }],
        );
    }

    if let Some(allowable) = contract.max_allowable_von_mises_mpa
        && (!allowable.is_finite() || allowable <= 0.0)
    {
        return build_unknown_evaluation(
            contract,
            None,
            None,
            "invalid max_allowable_von_mises_mpa (must be finite and > 0)",
            vec![FeaRepairHint {
                code: "FEA_INVALID_CONTRACT".to_owned(),
                recommended_direction: "set_positive_allowable_stress".to_owned(),
                summary: "Provide a positive finite max_allowable_von_mises_mpa in MPa.".to_owned(),
                measured_value: None,
                required_value: None,
                peak_stress_point_mm: None,
            }],
        );
    }

    let (material_label, material) =
        match resolve_fea_material(contract.material_id.as_deref(), contract.material) {
            Ok(resolved) => resolved,
            Err(err) => {
                return build_unknown_evaluation(
                    contract,
                    contract.material_id.clone(),
                    contract.material,
                    err,
                    vec![FeaRepairHint {
                        code: "FEA_UNKNOWN_MATERIAL".to_owned(),
                        recommended_direction: "assign_catalog_material".to_owned(),
                        summary: "Assign a valid material from schemas/materials.json (e.g. Al-6061-T6, SUS304, Ti-6Al-4V) or specify Young's modulus, Poisson's ratio, and yield strength.".to_owned(),
                        measured_value: None,
                        required_value: None,
                        peak_stress_point_mm: None,
                    }],
                );
            }
        };

    if contract.constraints.is_empty() || contract.loads.is_empty() {
        let missing = match (contract.constraints.is_empty(), contract.loads.is_empty()) {
            (true, true) => "fixed support constraints and force loads",
            (true, false) => "fixed support constraints (unconstrained rigid body)",
            (false, true) => "external force loads",
            (false, false) => unreachable!(),
        };
        return build_unknown_evaluation(
            contract,
            Some(material_label),
            Some(material),
            format!("incomplete structural boundary conditions: missing {missing}"),
            vec![FeaRepairHint {
                code: "FEA_INCOMPLETE_BOUNDARY_CONDITIONS".to_owned(),
                recommended_direction: "define_fixed_support_and_load_regions".to_owned(),
                summary: format!(
                    "Supply spatial {missing} so the 3D H8 voxel FEA solver can suppress rigid-body modes and solve K u = f."
                ),
                measured_value: None,
                required_value: None,
                peak_stress_point_mm: None,
            }],
        );
    }

    let max_voxels = contract
        .max_voxels_per_axis
        .clamp(MIN_VOXELS_PER_AXIS, MAX_VOXELS_PER_AXIS);
    let config = VoxelFeaConfig::new(
        material,
        contract.constraints.clone(),
        contract.loads.clone(),
    )
    .with_min_safety_factor(contract.min_safety_factor)
    .with_tolerance(contract.tolerance)
    .with_max_iters(contract.max_iters);

    let report = solve_mesh_voxel_fea_auto(triangles, max_voxels, &config);
    build_evaluation_from_report(contract, &material_label, material, report)
}

/// Extracts and evaluates an optional `L3_FEA` check from a `DesignContract`
/// and evaluated triangle scene.
pub(crate) fn evaluate_design_contract_fea(
    contract: Option<&DesignContract>,
    triangles: &[TriRef],
    fea_blob_declared: bool,
) -> Option<VerificationCheck> {
    let parsed = extract_contract_fea_request(contract, fea_blob_declared);
    if !parsed.requested {
        return None;
    }

    if let Some(reason) = parsed.incomplete_reason {
        let mut check = VerificationCheck::new(
            "L3_FEA",
            VerificationLayer::L3ManufacturabilityPhysics,
            TriVerdict::Unknown,
            reason,
        )
        .with_evidence("solver", SOLVER_NAME)
        .with_evidence("solver_mounted", true)
        .with_evidence("fea_blob_declared", fea_blob_declared)
        .with_evidence("declared_load_count", parsed.declared_load_count as u64)
        .with_evidence("constraint_count", parsed.contract.constraints.len() as u64)
        .with_evidence(
            "repair_hints",
            json!([FeaRepairHint {
                code: "FEA_INCOMPLETE_BOUNDARY_CONDITIONS".to_owned(),
                recommended_direction: "define_fixed_support_and_load_regions".to_owned(),
                summary: "Declare both fixed_support constraints and force loads with explicit spatial regions (box_region, face_plane, or all_nodes_within) in DesignContract.attributes.".to_owned(),
                measured_value: None,
                required_value: None,
                peak_stress_point_mm: None,
            }]),
        );
        if let Some(ref mat_id) = parsed.contract.material_id {
            check = check.with_evidence("material_id", mat_id.clone());
        }
        return Some(check);
    }

    let mut eval = evaluate_l3_fea_triangles(triangles, &parsed.contract);
    eval.check = eval
        .check
        .with_evidence("fea_blob_declared", fea_blob_declared)
        .with_evidence("declared_load_count", parsed.declared_load_count as u64)
        .with_evidence("constraint_count", parsed.contract.constraints.len() as u64);
    Some(eval.check)
}

fn triangles_from_scene_mesh(scene_mesh: &SceneMeshPayload) -> Vec<TriRef> {
    let mut triangles = Vec::new();
    for (face_idx, face) in scene_mesh.faces.iter().enumerate() {
        let face_slot_id = u32::try_from(face_idx).unwrap_or(u32::MAX);
        let vertices: Vec<[f64; 3]> = face
            .vertices
            .chunks_exact(3)
            .map(|pt| [pt[0], pt[1], pt[2]])
            .collect();
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
            triangles.push(TriRef {
                instance_id: 0,
                face_slot_id,
                a,
                b,
                c,
            });
        }
    }
    triangles
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

fn build_evaluation_from_report(
    contract: &StructuralFeaContract,
    material_label: &str,
    material: FeaMaterial,
    report: VoxelFeaReport,
) -> L3FeaEvaluation {
    let sf_allowable = material.yield_strength_mpa / contract.min_safety_factor;
    let allowable_von_mises_mpa = contract
        .max_allowable_von_mises_mpa
        .map_or(sf_allowable, |cap| cap.min(sf_allowable));

    let finite_sf = report
        .min_safety_factor
        .is_finite()
        .then_some(report.min_safety_factor);
    let finite_residual = report
        .relative_residual
        .is_finite()
        .then_some(report.relative_residual);

    let (verdict, message, repair_hints) = match report.verdict {
        FeaGateVerdict::Unknown => {
            let code = if report.reason.contains("rigid-body")
                || report.reason.contains("under-constrained")
                || report.reason.contains("disconnected")
            {
                "FEA_RIGID_BODY_INSTABILITY"
            } else if report.reason.contains("converge") {
                "FEA_SOLVER_NON_CONVERGENT"
            } else {
                "FEA_SOLVER_UNKNOWN"
            };
            let direction = if code == "FEA_RIGID_BODY_INSTABILITY" {
                "constrain_all_6_rigid_body_dofs"
            } else if code == "FEA_SOLVER_NON_CONVERGENT" {
                "refine_voxel_grid_or_increase_max_iters"
            } else {
                "check_fea_boundary_conditions"
            };
            (
                TriVerdict::Unknown,
                format!("L3 voxel FEA could not certify structure: {}", report.reason),
                vec![FeaRepairHint {
                    code: code.to_owned(),
                    recommended_direction: direction.to_owned(),
                    summary: report.reason.clone(),
                    measured_value: None,
                    required_value: None,
                    peak_stress_point_mm: report.peak_stress_point_mm,
                }],
            )
        }
        FeaGateVerdict::Fail => {
            let hint = FeaRepairHint {
                code: "FEA_INSUFFICIENT_SAFETY_FACTOR".to_owned(),
                recommended_direction: "increase_section_thickness_or_add_ribs".to_owned(),
                summary: format!(
                    "Peak Von Mises stress {:.2} MPa gives safety factor {:.2} < required {:.2} (material {material_label}, yield {:.1} MPa); thicken load-bearing section, add gussets/ribs, or upgrade material.",
                    report.max_von_mises_mpa,
                    report.min_safety_factor,
                    contract.min_safety_factor,
                    material.yield_strength_mpa
                ),
                measured_value: finite_sf,
                required_value: Some(contract.min_safety_factor),
                peak_stress_point_mm: report.peak_stress_point_mm,
            };
            (
                TriVerdict::Fail,
                format!(
                    "L3 voxel FEA failed ({material_label}): peak Von Mises {:.2} MPa, safety factor {:.2} < {:.2}, max displacement {:.4} mm",
                    report.max_von_mises_mpa,
                    report.min_safety_factor,
                    contract.min_safety_factor,
                    report.max_displacement_mm
                ),
                vec![hint],
            )
        }
        FeaGateVerdict::Pass => {
            if let Some(cap_mpa) = contract.max_allowable_von_mises_mpa
                && report.max_von_mises_mpa > cap_mpa
            {
                let hint = FeaRepairHint {
                    code: "FEA_STRESS_EXCEEDS_ALLOWABLE".to_owned(),
                    recommended_direction: "increase_section_thickness_or_add_ribs".to_owned(),
                    summary: format!(
                        "Peak Von Mises stress {:.2} MPa exceeds contract allowable limit {:.2} MPa; thicken critical section or add structural ribs near peak stress location.",
                        report.max_von_mises_mpa, cap_mpa
                    ),
                    measured_value: Some(report.max_von_mises_mpa),
                    required_value: Some(cap_mpa),
                    peak_stress_point_mm: report.peak_stress_point_mm,
                };
                (
                    TriVerdict::Fail,
                    format!(
                        "L3 voxel FEA failed ({material_label}): peak Von Mises {:.2} MPa > allowable {:.2} MPa (max displacement {:.4} mm)",
                        report.max_von_mises_mpa, cap_mpa, report.max_displacement_mm
                    ),
                    vec![hint],
                )
            } else {
                let sf_text = finite_sf
                    .map(|sf| format!("{sf:.2}"))
                    .unwrap_or_else(|| "inf".to_owned());
                (
                    TriVerdict::Pass,
                    format!(
                        "L3 voxel FEA passed ({material_label}): peak Von Mises {:.2} MPa, safety factor {sf_text} >= {:.2}, max displacement {:.4} mm",
                        report.max_von_mises_mpa,
                        contract.min_safety_factor,
                        report.max_displacement_mm
                    ),
                    Vec::new(),
                )
            }
        }
    };

    let summary = FeaGateSummaryPayload {
        verdict,
        solver: SOLVER_NAME.to_owned(),
        solver_mounted: true,
        material_id: Some(material_label.to_owned()),
        youngs_modulus_mpa: Some(material.youngs_modulus_mpa),
        poissons_ratio: Some(material.poissons_ratio),
        yield_strength_mpa: Some(material.yield_strength_mpa),
        required_safety_factor: contract.min_safety_factor,
        allowable_von_mises_mpa: Some(allowable_von_mises_mpa),
        converged: report.converged,
        iterations: report.iterations,
        relative_residual: finite_residual,
        active_voxel_count: report.active_voxel_count,
        active_dof_count: report.active_dof_count,
        max_displacement_mm: report.max_displacement_mm,
        max_von_mises_mpa: report.max_von_mises_mpa,
        safety_factor: finite_sf,
        peak_stress_point_mm: report.peak_stress_point_mm,
        repair_hints: repair_hints.clone(),
    };

    let mut check = VerificationCheck::new(
        "L3_FEA",
        VerificationLayer::L3ManufacturabilityPhysics,
        verdict,
        message,
    )
    .with_evidence("solver", SOLVER_NAME)
    .with_evidence("solver_mounted", true)
    .with_evidence("material_id", material_label.to_owned())
    .with_evidence("youngs_modulus_mpa", material.youngs_modulus_mpa)
    .with_evidence("poissons_ratio", material.poissons_ratio)
    .with_evidence("yield_strength_mpa", material.yield_strength_mpa)
    .with_evidence("required_safety_factor", contract.min_safety_factor)
    .with_evidence("allowable_von_mises_mpa", allowable_von_mises_mpa)
    .with_evidence("converged", report.converged)
    .with_evidence("iterations", report.iterations as u64)
    .with_evidence("active_voxel_count", report.active_voxel_count as u64)
    .with_evidence("active_dof_count", report.active_dof_count as u64)
    .with_evidence("max_displacement_mm", report.max_displacement_mm)
    .with_evidence("max_von_mises_mpa", report.max_von_mises_mpa);

    if let Some(sf) = finite_sf {
        check = check.with_evidence("safety_factor", sf);
    }
    if let Some(res) = finite_residual {
        check = check.with_evidence("relative_residual", res);
    }
    if let Some(pt) = report.peak_stress_point_mm {
        check = check.with_evidence("peak_stress_point_mm", json!(pt));
    }
    if !repair_hints.is_empty() {
        check = check.with_evidence("repair_hints", json!(repair_hints));
    }

    L3FeaEvaluation {
        check,
        summary,
        voxel_von_mises_mpa: report.voxel_von_mises_mpa,
    }
}

fn build_unknown_evaluation(
    contract: &StructuralFeaContract,
    material_id: Option<String>,
    material: Option<FeaMaterial>,
    reason: impl Into<String>,
    repair_hints: Vec<FeaRepairHint>,
) -> L3FeaEvaluation {
    let reason = reason.into();
    let allowable = material.and_then(|mat| {
        (contract.min_safety_factor.is_finite() && contract.min_safety_factor > 0.0).then(|| {
            let sf_cap = mat.yield_strength_mpa / contract.min_safety_factor;
            contract
                .max_allowable_von_mises_mpa
                .filter(|v| v.is_finite() && *v > 0.0)
                .map_or(sf_cap, |cap| cap.min(sf_cap))
        })
    });
    let summary = FeaGateSummaryPayload {
        verdict: TriVerdict::Unknown,
        solver: SOLVER_NAME.to_owned(),
        solver_mounted: true,
        material_id: material_id.clone(),
        youngs_modulus_mpa: material.map(|m| m.youngs_modulus_mpa),
        poissons_ratio: material.map(|m| m.poissons_ratio),
        yield_strength_mpa: material.map(|m| m.yield_strength_mpa),
        required_safety_factor: contract.min_safety_factor,
        allowable_von_mises_mpa: allowable,
        converged: false,
        iterations: 0,
        relative_residual: None,
        active_voxel_count: 0,
        active_dof_count: 0,
        max_displacement_mm: 0.0,
        max_von_mises_mpa: 0.0,
        safety_factor: None,
        peak_stress_point_mm: None,
        repair_hints: repair_hints.clone(),
    };
    let mut check = VerificationCheck::new(
        "L3_FEA",
        VerificationLayer::L3ManufacturabilityPhysics,
        TriVerdict::Unknown,
        reason,
    )
    .with_evidence("solver", SOLVER_NAME)
    .with_evidence("solver_mounted", true)
    .with_evidence("required_safety_factor", contract.min_safety_factor);
    if let Some(mat_id) = material_id {
        check = check.with_evidence("material_id", mat_id);
    }
    if !repair_hints.is_empty() {
        check = check.with_evidence("repair_hints", json!(repair_hints));
    }
    L3FeaEvaluation {
        check,
        summary,
        voxel_von_mises_mpa: Vec::new(),
    }
}

struct ParsedContractFeaRequest {
    requested: bool,
    declared_load_count: usize,
    contract: StructuralFeaContract,
    incomplete_reason: Option<String>,
}

fn extract_contract_fea_request(
    contract: Option<&DesignContract>,
    fea_blob_declared: bool,
) -> ParsedContractFeaRequest {
    let mut parsed = ParsedContractFeaRequest {
        requested: fea_blob_declared,
        declared_load_count: 0,
        contract: StructuralFeaContract::default(),
        incomplete_reason: None,
    };

    let Some(contract) = contract else {
        if fea_blob_declared {
            parsed.incomplete_reason = Some(
                "FEA field node declared without DesignContract structural boundary conditions"
                    .to_owned(),
            );
        }
        return parsed;
    };

    parsed.contract.material_id = contract.material.clone();
    let attrs = &contract.attributes;

    // 1. Direct structured `fea_config` / `structural_fea` JSON payload.
    if let Some(raw_cfg) = attrs.get("fea_config").or_else(|| attrs.get("structural_fea")) {
        let trimmed = raw_cfg.trim();
        if !trimmed.is_empty() {
            parsed.requested = true;
            match serde_json::from_str::<StructuralFeaContract>(trimmed) {
                Ok(mut cfg) => {
                    if cfg.material_id.is_none() {
                        cfg.material_id = contract.material.clone();
                    }
                    parsed.declared_load_count =
                        parsed.declared_load_count.saturating_add(cfg.loads.len());
                    parsed.contract = cfg;
                }
                Err(err) => {
                    parsed.incomplete_reason =
                        Some(format!("invalid fea_config JSON in DesignContract: {err}"));
                    return parsed;
                }
            }
        }
    }

    // 2. Flat scalar overrides in `DesignContract.attributes`.
    if let Some(sf) = parse_positive_f64_attr(attrs, "min_safety_factor") {
        parsed.contract.min_safety_factor = sf;
    }
    if let Some(allowable) = parse_positive_f64_attr(attrs, "max_allowable_von_mises_mpa")
        .or_else(|| parse_positive_f64_attr(attrs, "allowable_stress_mpa"))
    {
        parsed.contract.max_allowable_von_mises_mpa = Some(allowable);
    }
    if let Some(voxels) = attrs
        .get("fea_max_voxels_per_axis")
        .or_else(|| attrs.get("max_voxels_per_axis"))
        .and_then(|s| s.trim().parse::<usize>().ok())
        && voxels > 0
    {
        parsed.contract.max_voxels_per_axis = voxels;
    }

    // 3. Explicit constraints / fixed_supports JSON array.
    if let Some(raw_constraints) = attrs
        .get("constraints")
        .or_else(|| attrs.get("fixed_supports"))
        .or_else(|| attrs.get("boundary_conditions"))
    {
        let trimmed = raw_constraints.trim();
        if !trimmed.is_empty() {
            parsed.requested = true;
            match parse_constraints_json(trimmed) {
                Ok(items) => parsed.contract.constraints.extend(items),
                Err(err) => {
                    parsed.incomplete_reason = Some(err);
                    return parsed;
                }
            }
        }
    }

    // 4. `loads` JSON array (matches schemas/design-contract.schema.json and supports
    //    both `kind: "fixed_support"` and `kind: "force"` with spatial `region`).
    let mut unmapped_load_entries = 0usize;
    if let Some(raw_loads) = attrs.get("loads") {
        let trimmed = raw_loads.trim();
        if !trimmed.is_empty() {
            match serde_json::from_str::<Value>(trimmed) {
                Ok(Value::Array(items)) => {
                    if !items.is_empty() {
                        parsed.requested = true;
                    }
                    for (idx, item) in items.iter().enumerate() {
                        let Some(obj) = item.as_object() else {
                            parsed.declared_load_count =
                                parsed.declared_load_count.saturating_add(1);
                            unmapped_load_entries += 1;
                            continue;
                        };
                        let kind = obj
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("force")
                            .to_ascii_lowercase();
                        if let Some(sf) = obj.get("min_safety_factor").and_then(Value::as_f64)
                            && sf.is_finite()
                            && sf > 0.0
                        {
                            parsed.contract.min_safety_factor =
                                parsed.contract.min_safety_factor.max(sf);
                        }
                        if kind == "fixed_support" || kind == "fixed" || kind == "constraint" {
                            if let Ok(constraint) = parse_constraint_object(obj, idx) {
                                parsed.contract.constraints.push(constraint);
                            } else {
                                unmapped_load_entries += 1;
                            }
                        } else if kind == "force" {
                            parsed.declared_load_count =
                                parsed.declared_load_count.saturating_add(1);
                            if let Ok(load) = parse_force_load_object(obj, idx) {
                                parsed.contract.loads.push(load);
                            } else {
                                unmapped_load_entries += 1;
                            }
                        } else {
                            parsed.declared_load_count =
                                parsed.declared_load_count.saturating_add(1);
                            unmapped_load_entries += 1;
                        }
                    }
                }
                Ok(Value::Null) => {}
                _ => {
                    parsed.declared_load_count = parsed.declared_load_count.saturating_add(1);
                    parsed.requested = true;
                    unmapped_load_entries += 1;
                }
            }
        }
    }

    if attrs
        .get("fea_required")
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
    {
        parsed.requested = true;
    }

    if parsed.requested && unmapped_load_entries > 0 {
        parsed.incomplete_reason = Some(format!(
            "structural loads declared ({} unmapped entry/entries missing spatial region or unsupported load kind); L3 voxel FEA requires explicit fixed_support and force regions",
            unmapped_load_entries
        ));
    } else if parsed.requested
        && (parsed.contract.constraints.is_empty() || parsed.contract.loads.is_empty())
    {
        parsed.incomplete_reason = Some(
            "structural FEA requested without complete fixed_support constraints and force loads"
                .to_owned(),
        );
    }

    parsed
}

fn parse_inline_material_object(
    obj: &serde_json::Map<String, Value>,
) -> Result<(String, FeaMaterial), String> {
    let name = obj
        .get("name")
        .or_else(|| obj.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("custom")
        .to_owned();
    let youngs_modulus_mpa = obj
        .get("youngs_modulus_mpa")
        .and_then(Value::as_f64)
        .or_else(|| {
            obj.get("elastic_modulus_gpa")
                .or_else(|| obj.get("youngs_modulus_gpa"))
                .and_then(Value::as_f64)
                .map(|gpa| gpa * 1000.0)
        })
        .ok_or_else(|| {
            format!("inline material `{name}` missing youngs_modulus_mpa or elastic_modulus_gpa")
        })?;
    let poissons_ratio = obj
        .get("poissons_ratio")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("inline material `{name}` missing poissons_ratio"))?;
    let yield_strength_mpa = obj
        .get("yield_strength_mpa")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("inline material `{name}` missing yield_strength_mpa"))?;
    let mat = FeaMaterial {
        youngs_modulus_mpa,
        poissons_ratio,
        yield_strength_mpa,
    };
    if !mat.is_valid() {
        return Err(format!("inline material `{name}` has invalid elastic constants"));
    }
    Ok((name, mat))
}

fn parse_constraints_json(raw: &str) -> Result<Vec<FixedConstraint>, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| format!("invalid constraints JSON: {err}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "constraints must be a JSON array".to_owned())?;
    let mut constraints = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        if let Ok(direct) = serde_json::from_value::<FixedConstraint>(item.clone()) {
            constraints.push(direct);
            continue;
        }
        let obj = item
            .as_object()
            .ok_or_else(|| format!("constraints[{idx}] must be an object"))?;
        constraints.push(parse_constraint_object(obj, idx)?);
    }
    Ok(constraints)
}

fn parse_constraint_object(
    obj: &serde_json::Map<String, Value>,
    index: usize,
) -> Result<FixedConstraint, String> {
    let region_val = obj
        .get("region")
        .ok_or_else(|| format!("constraint[{index}] missing `region`"))?;
    let region = parse_fea_region(region_val)?;
    let fix_x = obj.get("fix_x").and_then(Value::as_bool).unwrap_or(true);
    let fix_y = obj.get("fix_y").and_then(Value::as_bool).unwrap_or(true);
    let fix_z = obj.get("fix_z").and_then(Value::as_bool).unwrap_or(true);
    Ok(FixedConstraint {
        region,
        fix_x,
        fix_y,
        fix_z,
    })
}

fn parse_force_load_object(
    obj: &serde_json::Map<String, Value>,
    index: usize,
) -> Result<ForceLoad, String> {
    let region_val = obj
        .get("region")
        .ok_or_else(|| format!("force load[{index}] missing `region`"))?;
    let region = parse_fea_region(region_val)?;
    let raw_vec = obj
        .get("total_force_n")
        .or_else(|| obj.get("vector"))
        .and_then(parse_vec3_value)
        .ok_or_else(|| format!("force load[{index}] missing `total_force_n` or `vector`"))?;
    let scale = match obj.get("unit").and_then(Value::as_str) {
        Some("kN") | Some("kn") => 1000.0,
        _ => 1.0,
    };
    let total_force_n = [raw_vec[0] * scale, raw_vec[1] * scale, raw_vec[2] * scale];
    Ok(ForceLoad {
        region,
        total_force_n,
    })
}

fn parse_fea_region(value: &Value) -> Result<FeaRegion, String> {
    if let Ok(direct) = serde_json::from_value::<FeaRegion>(value.clone()) {
        return Ok(direct);
    }
    let obj = value
        .as_object()
        .ok_or_else(|| "FEA region must be an object".to_owned())?;
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "FEA region missing `kind`".to_owned())?
        .to_ascii_lowercase();
    match kind.as_str() {
        "box" | "aabb" | "box_region" => {
            let min_mm = obj
                .get("min_mm")
                .or_else(|| obj.get("min"))
                .and_then(parse_vec3_value)
                .ok_or_else(|| "box region missing `min_mm`".to_owned())?;
            let max_mm = obj
                .get("max_mm")
                .or_else(|| obj.get("max"))
                .and_then(parse_vec3_value)
                .ok_or_else(|| "box region missing `max_mm`".to_owned())?;
            Ok(FeaRegion::BoxRegion { min_mm, max_mm })
        }
        "plane" | "face" | "face_plane" => {
            let normal = obj
                .get("normal")
                .and_then(parse_vec3_value)
                .ok_or_else(|| "face_plane region missing `normal`".to_owned())?;
            let offset_mm = obj
                .get("offset_mm")
                .and_then(Value::as_f64)
                .ok_or_else(|| "face_plane region missing `offset_mm`".to_owned())?;
            let tol_mm = obj
                .get("tol_mm")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            Ok(FeaRegion::FacePlane {
                normal,
                offset_mm,
                tol_mm,
            })
        }
        "sphere" | "all_nodes_within" => {
            let center_mm = obj
                .get("center_mm")
                .or_else(|| obj.get("origin_mm"))
                .and_then(parse_vec3_value)
                .ok_or_else(|| "sphere region missing `center_mm`".to_owned())?;
            let radius_mm = obj
                .get("radius_mm")
                .and_then(Value::as_f64)
                .ok_or_else(|| "sphere region missing `radius_mm`".to_owned())?;
            Ok(FeaRegion::AllNodesWithin {
                center_mm,
                radius_mm,
            })
        }
        other => Err(format!("unsupported FEA region kind `{other}`")),
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

fn parse_positive_f64_attr(attrs: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    let value: f64 = attrs.get(key)?.trim().parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

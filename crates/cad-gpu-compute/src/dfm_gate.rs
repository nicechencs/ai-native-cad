//! High-level pluggable DFM & Interference Gate API with structured repair hints.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::backend::GpuComputeBackend;
use crate::dfm_raybeam::{DfmConfig, DfmReport, DfmViolationKind, evaluate_raybeam_dfm};
use crate::interference::InterferenceReport;
use crate::keepout::{KeepOutReport, KeepOutZone};
use crate::lbvh::{Aabb3, TriRef};
use crate::roi::RoiAnchor;

/// Categorized actionable repair direction emitted when a DFM or interference check fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DfmRepairKind {
    IncreaseWallThickness,
    IncreaseDraftAngle,
    ImproveToolAccessibility,
    ClearKeepOutZone,
    ResolveBodyInterference,
}

/// Machine-readable repair hint consumed by L3 verification reports and AI repair loops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DfmRepairHint {
    pub kind: DfmRepairKind,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_slot_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_value: Option<f64>,
    pub unit: String,
    pub recommended_direction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_hint: Option<f64>,
    pub message: String,
}

/// Unified input for running ray-beam DFM, Keep-Out, and multi-body interference gates.
#[derive(Clone, Debug)]
pub struct DfmAnalysisRequest {
    pub triangles: Vec<TriRef>,
    pub config: DfmConfig,
    pub keep_out_zones: Vec<KeepOutZone>,
    /// Minimum safety clearance between distinct instances. When `None`, defaults to `0.0`
    /// if `check_multi_body_interference` is enabled.
    pub min_clearance_mm: Option<f64>,
    /// Enables LBVH multi-body interference and containment screening when two or
    /// more distinct instances are present.
    pub check_multi_body_interference: bool,
    /// Optional human/topology face identifiers keyed by `face_slot_id`.
    pub face_labels: BTreeMap<u32, String>,
    /// Optional part/node identifiers keyed by `instance_id`.
    pub instance_labels: BTreeMap<u32, String>,
}

impl DfmAnalysisRequest {
    /// Builds a request with sensible defaults for wall-thickness ray-beam analysis.
    #[must_use]
    pub fn new(triangles: Vec<TriRef>, config: DfmConfig) -> Self {
        Self {
            triangles,
            config,
            keep_out_zones: Vec::new(),
            min_clearance_mm: None,
            check_multi_body_interference: true,
            face_labels: BTreeMap::new(),
            instance_labels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_keep_out_zones(mut self, zones: Vec<KeepOutZone>) -> Self {
        self.keep_out_zones = zones;
        self
    }

    #[must_use]
    pub fn with_min_clearance_mm(mut self, min_clearance_mm: f64) -> Self {
        self.min_clearance_mm = Some(min_clearance_mm.max(0.0));
        self.check_multi_body_interference = true;
        self
    }

    #[must_use]
    pub fn with_face_labels(mut self, face_labels: BTreeMap<u32, String>) -> Self {
        self.face_labels = face_labels;
        self
    }

    #[must_use]
    pub fn with_instance_labels(mut self, instance_labels: BTreeMap<u32, String>) -> Self {
        self.instance_labels = instance_labels;
        self
    }
}

/// Complete deterministic DFM, Keep-Out, and interference gate summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DfmGateSummary {
    pub passed: bool,
    pub backend: String,
    pub roi_origin_mm: [f64; 3],
    pub triangle_count: usize,
    pub face_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_measured_wall_mm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_measured_draft_deg: Option<f64>,
    pub thin_wall_face_count: usize,
    pub draft_violation_face_count: usize,
    pub inaccessible_face_count: usize,
    pub keepout_violation_count: usize,
    pub interference_pair_count: usize,
    pub dfm_report: DfmReport,
    pub keepout_report: KeepOutReport,
    pub interference_report: InterferenceReport,
    pub repair_hints: Vec<DfmRepairHint>,
}

/// Evaluates `request` using the shared auto-selected GPU/CPU compute backend.
#[must_use]
pub fn run_dfm_gate(request: &DfmAnalysisRequest) -> DfmGateSummary {
    run_dfm_gate_with_backend(GpuComputeBackend::shared_auto(), request)
}

/// Evaluates `request` using an explicit [`GpuComputeBackend`] instance.
#[must_use]
pub fn run_dfm_gate_with_backend(
    backend: &GpuComputeBackend,
    request: &DfmAnalysisRequest,
) -> DfmGateSummary {
    let zone_aabbs: Vec<Aabb3> = request
        .keep_out_zones
        .iter()
        .filter_map(KeepOutZone::aabb)
        .collect();
    let anchor = RoiAnchor::from_triangles_and_aabbs(&request.triangles, &zone_aabbs);

    let (dfm_report, active_backend) =
        match backend.analyze_dfm_raybeams(&request.triangles, &request.config) {
            Ok(report) => (report, backend.backend_name().to_owned()),
            Err(_) => (
                evaluate_raybeam_dfm(&request.triangles, &request.config),
                "cpu-lbvh".to_owned(),
            ),
        };

    let keepout_report = if request.keep_out_zones.is_empty() {
        KeepOutReport::default()
    } else {
        backend.check_keepout(&request.triangles, &request.keep_out_zones)
    };

    let distinct_instances: BTreeSet<u32> = request
        .triangles
        .iter()
        .map(|tri| tri.instance_id)
        .collect();
    let min_clearance_mm = request.min_clearance_mm.unwrap_or(0.0).max(0.0);
    let interference_report = if (request.check_multi_body_interference || request.min_clearance_mm.is_some())
        && distinct_instances.len() >= 2
    {
        backend.check_interference(&request.triangles, min_clearance_mm)
    } else {
        InterferenceReport::default()
    };

    let face_to_instance: BTreeMap<u32, u32> = request
        .triangles
        .iter()
        .map(|tri| (tri.face_slot_id, tri.instance_id))
        .collect();

    let face_label_for = |slot: u32| -> String {
        request
            .face_labels
            .get(&slot)
            .cloned()
            .unwrap_or_else(|| format!("face_slot_{slot}"))
    };

    let mut min_measured_wall_mm: Option<f64> = None;
    let mut min_measured_draft_deg: Option<f64> = None;
    let mut thin_wall_face_count = 0usize;
    let mut draft_violation_face_count = 0usize;
    let mut inaccessible_face_count = 0usize;
    let mut repair_hints = Vec::new();

    for face in &dfm_report.faces {
        if let Some(thickness) = face.min_thickness_mm {
            min_measured_wall_mm =
                Some(min_measured_wall_mm.map_or(thickness, |cur| cur.min(thickness)));
        }
        if let Some(draft) = face.draft_angle_deg {
            min_measured_draft_deg =
                Some(min_measured_draft_deg.map_or(draft, |cur| cur.min(draft)));
        }

        let face_id = face_label_for(face.face_slot_id);
        let instance_id = face_to_instance.get(&face.face_slot_id).copied();

        for violation in &face.violations {
            match violation {
                DfmViolationKind::ThinWall => {
                    thin_wall_face_count += 1;
                    let measured = face.min_thickness_mm;
                    let required = request.config.min_wall_thickness_mm;
                    let delta = measured.map(|m| (required - m).max(0.0));
                    repair_hints.push(DfmRepairHint {
                        kind: DfmRepairKind::IncreaseWallThickness,
                        code: "DFM_THIN_WALL".to_owned(),
                        face_id: Some(face_id.clone()),
                        face_slot_id: Some(face.face_slot_id),
                        instance_id,
                        zone_id: None,
                        measured_value: measured,
                        required_value: Some(required),
                        unit: "mm".to_owned(),
                        recommended_direction: "increase_wall_thickness".to_owned(),
                        delta_hint: delta,
                        message: format!(
                            "face `{face_id}` measured wall thickness {:.4} mm is below required {:.4} mm; increase wall thickness by at least {:.4} mm",
                            measured.unwrap_or(0.0),
                            required,
                            delta.unwrap_or(required),
                        ),
                    });
                }
                DfmViolationKind::DraftAngle => {
                    draft_violation_face_count += 1;
                    let measured = face.draft_angle_deg;
                    let required = request.config.min_draft_angle_deg;
                    let delta = measured.map(|m| (required - m).max(0.0));
                    repair_hints.push(DfmRepairHint {
                        kind: DfmRepairKind::IncreaseDraftAngle,
                        code: "DFM_DRAFT_ANGLE".to_owned(),
                        face_id: Some(face_id.clone()),
                        face_slot_id: Some(face.face_slot_id),
                        instance_id,
                        zone_id: None,
                        measured_value: measured,
                        required_value: Some(required),
                        unit: "deg".to_owned(),
                        recommended_direction: "increase_draft_angle".to_owned(),
                        delta_hint: delta,
                        message: format!(
                            "face `{face_id}` measured draft angle {:.2} deg is below required {:.2} deg; add draft along pull direction",
                            measured.unwrap_or(0.0),
                            required,
                        ),
                    });
                }
                DfmViolationKind::ToolAccessibility => {
                    inaccessible_face_count += 1;
                    repair_hints.push(DfmRepairHint {
                        kind: DfmRepairKind::ImproveToolAccessibility,
                        code: "DFM_TOOL_ACCESSIBILITY".to_owned(),
                        face_id: Some(face_id.clone()),
                        face_slot_id: Some(face.face_slot_id),
                        instance_id,
                        zone_id: None,
                        measured_value: None,
                        required_value: None,
                        unit: "axis".to_owned(),
                        recommended_direction: "open_pocket_or_add_tool_axis".to_owned(),
                        delta_hint: None,
                        message: format!(
                            "face `{face_id}` is obstructed along all configured tool axes; remove overhang or add a machining setup axis"
                        ),
                    });
                }
            }
        }
    }

    for violation in &keepout_report.violations {
        let face_id = violation.face_slot_id.map(face_label_for);
        let inst_label = request
            .instance_labels
            .get(&violation.instance_id)
            .cloned()
            .unwrap_or_else(|| format!("instance_{}", violation.instance_id));
        let detail = if violation.enclosed {
            format!(
                "keep-out zone `{}` is wholly enclosed inside `{inst_label}` (witness [{:.3}, {:.3}, {:.3}] mm); carve clearance around zone",
                violation.zone_id,
                violation.witness_point_mm[0],
                violation.witness_point_mm[1],
                violation.witness_point_mm[2],
            )
        } else {
            format!(
                "face `{}` on `{inst_label}` intrudes keep-out zone `{}` by {:.4} mm at [{:.3}, {:.3}, {:.3}] mm; retract geometry away from keep-out zone",
                face_id.as_deref().unwrap_or("unknown"),
                violation.zone_id,
                violation.penetration_mm,
                violation.witness_point_mm[0],
                violation.witness_point_mm[1],
                violation.witness_point_mm[2],
            )
        };
        repair_hints.push(DfmRepairHint {
            kind: DfmRepairKind::ClearKeepOutZone,
            code: "DFM_KEEPOUT_INTRUSION".to_owned(),
            face_id,
            face_slot_id: violation.face_slot_id,
            instance_id: Some(violation.instance_id),
            zone_id: Some(violation.zone_id.clone()),
            measured_value: Some(violation.penetration_mm),
            required_value: Some(0.0),
            unit: "mm".to_owned(),
            recommended_direction: "retract_geometry_from_keepout".to_owned(),
            delta_hint: Some(violation.penetration_mm),
            message: detail,
        });
    }

    for pair in &interference_report.pairs {
        let label_a = request
            .instance_labels
            .get(&pair.instance_a)
            .cloned()
            .unwrap_or_else(|| format!("instance_{}", pair.instance_a));
        let label_b = request
            .instance_labels
            .get(&pair.instance_b)
            .cloned()
            .unwrap_or_else(|| format!("instance_{}", pair.instance_b));
        let face_id = face_label_for(pair.face_a);
        let delta = if pair.penetrating {
            min_clearance_mm.max(0.1)
        } else {
            (min_clearance_mm - pair.min_distance_mm).max(0.0)
        };
        repair_hints.push(DfmRepairHint {
            kind: DfmRepairKind::ResolveBodyInterference,
            code: "DFM_BODY_INTERFERENCE".to_owned(),
            face_id: Some(face_id.clone()),
            face_slot_id: Some(pair.face_a),
            instance_id: Some(pair.instance_a),
            zone_id: None,
            measured_value: Some(pair.min_distance_mm),
            required_value: Some(min_clearance_mm),
            unit: "mm".to_owned(),
            recommended_direction: "increase_body_clearance".to_owned(),
            delta_hint: Some(delta),
            message: if pair.penetrating {
                format!(
                    "body `{label_a}` (`{face_id}`) penetrates `{label_b}` (`{}`); separate or boolean-resolve colliding bodies",
                    face_label_for(pair.face_b),
                )
            } else {
                format!(
                    "clearance {:.4} mm between `{label_a}` (`{face_id}`) and `{label_b}` (`{}`) is below required {:.4} mm; increase separation by {:.4} mm",
                    pair.min_distance_mm,
                    face_label_for(pair.face_b),
                    min_clearance_mm,
                    delta,
                )
            },
        });
    }

    let keepout_violation_count = keepout_report.violation_count();
    let interference_pair_count = interference_report.pairs.len();
    let passed = dfm_report.is_clear()
        && keepout_report.is_clear()
        && interference_report.is_clear();

    DfmGateSummary {
        passed,
        backend: active_backend,
        roi_origin_mm: anchor.origin_mm,
        triangle_count: request.triangles.len(),
        face_count: dfm_report.faces.len(),
        min_measured_wall_mm,
        min_measured_draft_deg,
        thin_wall_face_count,
        draft_violation_face_count,
        inaccessible_face_count,
        keepout_violation_count,
        interference_pair_count,
        dfm_report,
        keepout_report,
        interference_report,
        repair_hints,
    }
}

//! Progressive-disclosure `aicad://` resources.
//!
//! Catalog numbers come from `cad-stdlib` lookups. Feature examples are real
//! `SemanticCommand` values. Neither list is inlined into `tools/list`.

use std::collections::BTreeMap;

use cad_api::{
    AddDesignAssertion, AddMate, BooleanOpKind, BooleanSolids, ChamferEdges, ComponentSourceSpec,
    CreateHole, CreatePrimitiveBox, CreatePrimitiveCylinder, CreateSketch, DatumPlane, DeleteNode,
    DesignAssertionKind, DesignContract, DimInput, DirectFaceEditCommand, DirectFaceEditOp,
    DraftFaces, ExtrudeSketch, FastenerClearance, FastenerHoleStandard, FeatureBodyOp,
    FilletEdges, HoleDefinition, HoleDepthSpec, HoleProfileSpec, InsertComponent, IntInput,
    LoftSections, MateKindSpec, MirrorFeature, PatternDefinition, PatternFeature, RemoveParameter,
    RenameNode, ReorderFeature, RevolveSketch, SemanticCommand, SetDesignContract,
    SetFeatureInput, SetNodeSuppressed, ShellSolid, SketchPlaneSpec, SweepPathInput, SweepProfile,
    TagTopologyCommand, UpsertParameter, UpsertSketchConstraints, UpsertSketchEntities,
};
use cad_stdlib::{
    ClearanceClass, lookup_bearing, lookup_iso10642_csink, lookup_iso286, lookup_iso4762_cbore,
    lookup_material, lookup_metric_tap_drill,
};
use serde_json::{Value, json};

use crate::error::GatewayError;

const FASTENER_SIZES: &[&str] = &["M2", "M2.5", "M3", "M4", "M5", "M6", "M8", "M10", "M12"];
const BEARINGS: &[&str] = &[
    "608ZZ", "6000", "6001", "6002", "6004", "6200", "6201", "6202", "6204", "6804", "6805",
];
const ISO_BANDS: &[&str] = &[
    "H6", "H7", "H8", "H9", "H11", "J7", "JS7", "g6", "f7", "h6", "h7", "js5", "k5", "m6", "p6",
];
const ISO_STEPS_MM: [f64; 10] = [3.0, 6.0, 10.0, 18.0, 30.0, 50.0, 80.0, 120.0, 180.0, 250.0];
const MATERIALS: &[&str] = &[
    "Al-6061-T6",
    "Al-7075-T6",
    "Steel-45",
    "Steel-4140",
    "SS-304",
    "SS-316L",
    "Ti-6Al-4V",
    "Brass-C360",
    "POM-Delrin",
    "PA12-SLS",
    "PEEK",
];

const COMMANDS: &[&str] = &[
    "upsert_parameter",
    "set_feature_input",
    "remove_parameter",
    "set_design_contract",
    "create_sketch",
    "upsert_sketch_entities",
    "upsert_sketch_constraints",
    "create_primitive_box",
    "create_primitive_cylinder",
    "extrude_sketch",
    "revolve_sketch",
    "sweep_profile",
    "loft_sections",
    "create_hole",
    "fillet_edges",
    "chamfer_edges",
    "shell_solid",
    "draft_faces",
    "pattern_feature",
    "mirror_feature",
    "boolean_solids",
    "direct_face_edit",
    "insert_component",
    "add_mate",
    "rename_node",
    "set_node_suppressed",
    "delete_node",
    "reorder_feature",
    "tag_topology",
    "add_design_assertion",
];

const FEATURE_ALIASES: &[(&str, &str)] = &[
    ("StdFastenerHole", "std_fastener_hole"),
    ("std_fastener_hole", "std_fastener_hole"),
    ("StructuredSketch2D", "structured_sketch"),
    ("structured_sketch_2d", "structured_sketch"),
    ("Sweep", "sweep_profile"),
    ("MateConstraint", "add_mate"),
    ("mate_constraint", "add_mate"),
];

pub fn list_resources() -> Value {
    let mut resources = vec![
        resource(
            "aicad://schema/semantic-commands",
            "semantic-commands",
            "SemanticCommand wire format. Open aicad://schema/feature/<kind> for one command.",
        ),
        resource(
            "aicad://stdlib/fasteners",
            "fasteners",
            "ISO fastener hole sizes from cad-stdlib.",
        ),
        resource(
            "aicad://stdlib/bearings",
            "bearings",
            "Bearing seat table from cad-stdlib.",
        ),
        resource(
            "aicad://stdlib/iso286",
            "iso286",
            "ISO 286 limit deviations from cad-stdlib.",
        ),
        resource(
            "aicad://stdlib/materials",
            "materials",
            "Material constants from cad-stdlib.",
        ),
        resource(
            "aicad://diagnostics/catalog",
            "diagnostics",
            "Recovery playbook for stable error codes.",
        ),
    ];
    for kind in COMMANDS {
        resources.push(resource(
            &format!("aicad://schema/feature/{kind}"),
            kind,
            "One SemanticCommand example.",
        ));
    }
    for (alias, _) in FEATURE_ALIASES {
        resources.push(resource(
            &format!("aicad://schema/feature/{alias}"),
            alias,
            "Feature alias. The example uses the SemanticCommand wire name.",
        ));
    }
    json!({ "resources": resources })
}

pub fn read_resource(uri: &str) -> Result<Value, GatewayError> {
    let body = match uri {
        "aicad://schema/semantic-commands" => semantic_commands(),
        "aicad://stdlib/fasteners" => fasteners().map_err(GatewayError::from)?,
        "aicad://stdlib/bearings" => bearings().map_err(GatewayError::from)?,
        "aicad://stdlib/iso286" => iso286().map_err(GatewayError::from)?,
        "aicad://stdlib/materials" => materials().map_err(GatewayError::from)?,
        "aicad://diagnostics/catalog" => diagnostics(),
        other => feature_resource(other)?,
    };
    let text = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_owned());
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": text,
        }]
    }))
}

fn resource(uri: &str, name: &str, description: &str) -> Value {
    json!({
        "uri": uri,
        "name": name,
        "description": description,
        "mimeType": "application/json",
    })
}

fn semantic_commands() -> Value {
    json!({
        "wire": "Adjacent tag: {\"command\":\"<kind>\",\"args\":{...}}.",
        "dim_input": {
            "literal_mm": {"source": "literal", "dimension": "length", "value": 1.0, "unit": "mm"},
            "param": {"source": "param", "param_id": "width"},
        },
        "commands": COMMANDS,
        "aliases": FEATURE_ALIASES.iter().map(|(alias, _)| alias).collect::<Vec<_>>(),
        "example_uri": "aicad://schema/feature/create_primitive_box",
        "minimal_example": serde_json::to_value(example_command("create_primitive_box")).ok(),
    })
}

fn feature_resource(uri: &str) -> Result<Value, GatewayError> {
    let kind = uri
        .strip_prefix("aicad://schema/feature/")
        .filter(|kind| !kind.is_empty() && !kind.contains('/'))
        .ok_or_else(|| GatewayError::invalid(format!("unknown resource `{uri}`")))?;
    let resolved = resolve_kind(kind).ok_or_else(|| {
        GatewayError::invalid(format!(
            "unknown feature `{kind}`. Read aicad://schema/semantic-commands."
        ))
    })?;
    let example = example_command(resolved);
    let command = example.command_name();
    Ok(json!({
        "kind": kind,
        "command": command,
        "example": example,
        "note": "Copy `example` into cad_apply_commands.commands. Do not invent catalog numbers; read aicad://stdlib/.",
    }))
}

fn resolve_kind(kind: &str) -> Option<&'static str> {
    if let Some(found) = COMMANDS.iter().copied().find(|item| *item == kind) {
        return Some(found);
    }
    FEATURE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == kind)
        .map(|(_, target)| *target)
}

fn example_command(kind: &str) -> SemanticCommand {
    let mm = |value: f64| DimInput::mm(value).expect("mm");
    let deg = |value: f64| DimInput::deg(value).expect("deg");
    match kind {
        "upsert_parameter" => SemanticCommand::UpsertParameter(
            UpsertParameter::length_mm("width", 80.0, None).expect("parameter"),
        ),
        "set_feature_input" => SemanticCommand::SetFeatureInput(SetFeatureInput {
            node_id: "plate".into(),
            input: "width".into(),
            length_mm: Some(24.0),
            vector_mm: None,
        }),
        "remove_parameter" => {
            SemanticCommand::RemoveParameter(RemoveParameter { name: "width".into() })
        }
        "set_design_contract" => SemanticCommand::SetDesignContract(SetDesignContract {
            contract: DesignContract {
                part_name: "bracket".into(),
                summary: None,
                material: Some("Al-6061-T6".into()),
                manufacturing_method: None,
                attributes: BTreeMap::new(),
            },
        }),
        "structured_sketch" | "create_sketch" => SemanticCommand::CreateSketch(CreateSketch {
            node_id: Some("plate_sketch".into()),
            name: "Plate sketch".into(),
            plane: SketchPlaneSpec::datum(DatumPlane::Xy),
            entities: Vec::new(),
            profiles: Vec::new(),
            constraints: Vec::new(),
        }),
        "upsert_sketch_entities" => {
            SemanticCommand::UpsertSketchEntities(UpsertSketchEntities {
                node_id: "plate_sketch".into(),
                entities: Vec::new(),
                replace: false,
            })
        }
        "upsert_sketch_constraints" => {
            SemanticCommand::UpsertSketchConstraints(UpsertSketchConstraints {
                node_id: "plate_sketch".into(),
                constraints: Vec::new(),
                replace: false,
            })
        }
        "create_primitive_box" => SemanticCommand::CreatePrimitiveBox(CreatePrimitiveBox {
            node_id: Some("plate".into()),
            name: "Plate".into(),
            width: mm(80.0),
            depth: mm(40.0),
            height: mm(8.0),
            origin_mm: [0.0, 0.0, 0.0],
        }),
        "create_primitive_cylinder" => {
            SemanticCommand::CreatePrimitiveCylinder(CreatePrimitiveCylinder {
                node_id: Some("hole_a".into()),
                name: "Hole A".into(),
                radius: mm(3.3),
                height: mm(16.0),
                origin_mm: [16.0, 20.0, -4.0],
                axis: [0.0, 0.0, 1.0],
            })
        }
        "extrude_sketch" => SemanticCommand::ExtrudeSketch(ExtrudeSketch {
            node_id: Some("plate".into()),
            name: "Plate".into(),
            sketch: "plate_sketch".into(),
            distance: mm(8.0),
            direction: None,
            symmetric: false,
            operation: FeatureBodyOp::NewBody,
            target: None,
        }),
        "revolve_sketch" => SemanticCommand::RevolveSketch(RevolveSketch {
            node_id: Some("hub".into()),
            name: "Hub".into(),
            sketch: "plate_sketch".into(),
            axis_origin_mm: [0.0, 0.0, 0.0],
            axis_direction: [0.0, 0.0, 1.0],
            angle: deg(360.0),
            operation: FeatureBodyOp::NewBody,
            target: None,
        }),
        "sweep_profile" => SemanticCommand::SweepProfile(SweepProfile {
            node_id: Some("sweep".into()),
            name: "Sweep".into(),
            profile: "plate_sketch".into(),
            path: SweepPathInput::Polyline {
                points_mm: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 20.0]],
            },
            keep_normal: true,
        }),
        "loft_sections" => SemanticCommand::LoftSections(LoftSections {
            node_id: Some("loft".into()),
            name: "Loft".into(),
            sections: vec!["section_a".into(), "section_b".into()],
            guides: Vec::new(),
            closed: false,
            as_surface: false,
        }),
        "std_fastener_hole" => SemanticCommand::CreateHole(CreateHole {
            node_id: Some("mount".into()),
            name: "M6 counterbore".into(),
            solid: "plate".into(),
            target_face: None,
            origin_mm: [20.0, 20.0, 8.0],
            axis: [0.0, 0.0, -1.0],
            definition: HoleDefinition::Standard {
                standard: FastenerHoleStandard::Iso4762Counterbore,
                designation: "M6".into(),
                clearance_class: Some(FastenerClearance::Normal),
                depth: HoleDepthSpec::ThroughAll,
                thread_depth: None,
            },
        }),
        "create_hole" => SemanticCommand::CreateHole(CreateHole {
            node_id: Some("hole_a".into()),
            name: "Hole A".into(),
            solid: "plate".into(),
            target_face: None,
            origin_mm: [16.0, 20.0, 8.0],
            axis: [0.0, 0.0, -1.0],
            definition: HoleDefinition::Generic {
                diameter: mm(6.6),
                depth: HoleDepthSpec::ThroughAll,
                profile: HoleProfileSpec::Simple,
            },
        }),
        "fillet_edges" => SemanticCommand::FilletEdges(FilletEdges {
            node_id: Some("fillet".into()),
            name: "Fillet".into(),
            solid: "plate".into(),
            edges: Vec::new(),
            radius: mm(1.0),
        }),
        "chamfer_edges" => SemanticCommand::ChamferEdges(ChamferEdges {
            node_id: Some("chamfer".into()),
            name: "Chamfer".into(),
            solid: "plate".into(),
            edges: Vec::new(),
            distance: mm(1.0),
            distance2: None,
            angle: None,
        }),
        "shell_solid" => SemanticCommand::ShellSolid(ShellSolid {
            node_id: Some("shell".into()),
            name: "Shell".into(),
            solid: "plate".into(),
            open_faces: Vec::new(),
            thickness: mm(2.0),
            inward: true,
        }),
        "draft_faces" => SemanticCommand::DraftFaces(DraftFaces {
            node_id: Some("draft".into()),
            name: "Draft".into(),
            solid: "plate".into(),
            faces: vec!["face:side".into()],
            pull_direction: [0.0, 0.0, 1.0],
            neutral_plane: SketchPlaneSpec::datum(DatumPlane::Xy),
            angle: deg(2.0),
        }),
        "pattern_feature" => SemanticCommand::PatternFeature(PatternFeature {
            node_id: Some("pattern".into()),
            name: "Pattern".into(),
            solid: "hole_a".into(),
            definition: PatternDefinition::Linear {
                direction: [1.0, 0.0, 0.0],
                count: IntInput::literal(2),
                spacing: mm(40.0),
                second_direction: None,
            },
        }),
        "mirror_feature" => SemanticCommand::MirrorFeature(MirrorFeature {
            node_id: Some("mirror".into()),
            name: "Mirror".into(),
            solid: "plate".into(),
            plane: SketchPlaneSpec::datum(DatumPlane::Yz),
            keep_original: true,
        }),
        "boolean_solids" => SemanticCommand::BooleanSolids(BooleanSolids {
            node_id: Some("bracket".into()),
            name: "Bracket".into(),
            op: BooleanOpKind::Subtract,
            target: "plate".into(),
            tools: vec!["hole_a".into(), "hole_b".into()],
        }),
        "direct_face_edit" => SemanticCommand::DirectFaceEdit(DirectFaceEditCommand {
            node_id: Some("move_face".into()),
            name: "Move face".into(),
            solid: "plate".into(),
            face: "face:top".into(),
            edit: DirectFaceEditOp::DeleteAndHeal,
        }),
        "insert_component" => SemanticCommand::InsertComponent(InsertComponent {
            node_id: Some("part".into()),
            name: "Part".into(),
            source: ComponentSourceSpec::LocalNode {
                node_id: "plate".into(),
            },
            translation_mm: [0.0, 0.0, 0.0],
            rotation_deg: [0.0, 0.0, 0.0],
            grounded: true,
        }),
        "add_mate" => SemanticCommand::AddMate(AddMate {
            node_id: Some("mate".into()),
            name: "Mate".into(),
            left: "plate".into(),
            right: "part".into(),
            left_ref: "face:top".into(),
            right_ref: "face:bottom".into(),
            mate_kind: MateKindSpec::Coincident,
            flip: false,
        }),
        "rename_node" => SemanticCommand::RenameNode(RenameNode {
            node_id: "plate".into(),
            name: "Bracket plate".into(),
        }),
        "set_node_suppressed" => SemanticCommand::SetNodeSuppressed(SetNodeSuppressed {
            node_id: "hole_a".into(),
            suppressed: false,
        }),
        "delete_node" => SemanticCommand::DeleteNode(DeleteNode {
            node_id: "hole_b".into(),
        }),
        "reorder_feature" => SemanticCommand::ReorderFeature(ReorderFeature {
            node_id: "hole_a".into(),
            before: None,
        }),
        "tag_topology" => SemanticCommand::TagTopology(TagTopologyCommand {
            node_id: Some("tag".into()),
            name: "Tag top".into(),
            solid: "plate".into(),
            tag: "top".into(),
            targets: Vec::new(),
        }),
        "add_design_assertion" => SemanticCommand::AddDesignAssertion(AddDesignAssertion {
            node_id: Some("assert_volume".into()),
            name: "Volume".into(),
            target: Some("bracket".into()),
            assertion_id: "volume".into(),
            kind: DesignAssertionKind::VolumeBounds {
                min_mm3: Some(1.0),
                max_mm3: Some(100_000.0),
            },
        }),
        _ => SemanticCommand::RenameNode(RenameNode {
            node_id: "plate".into(),
            name: "Plate".into(),
        }),
    }
}

fn fasteners() -> Result<Value, cad_stdlib::StdLibError> {
    let mut rows = Vec::new();
    for size in FASTENER_SIZES {
        let counterbore = lookup_iso4762_cbore(size)?;
        let countersink = lookup_iso10642_csink(size)?;
        let tap_drill_mm = lookup_metric_tap_drill(size)?;
        rows.push(json!({
            "size": size,
            "counterbore": counterbore,
            "countersink": countersink,
            "tap_drill_mm": tap_drill_mm,
            "clearance_normal_mm": counterbore.clearance_mm(ClearanceClass::Normal),
        }));
    }
    Ok(json!({
        "source": "cad-stdlib",
        "fasteners": rows,
    }))
}

fn bearings() -> Result<Value, cad_stdlib::StdLibError> {
    let mut rows = Vec::new();
    for designation in BEARINGS {
        rows.push(serde_json::to_value(lookup_bearing(designation)?).unwrap_or(Value::Null));
    }
    Ok(json!({ "source": "cad-stdlib", "bearings": rows }))
}

fn iso286() -> Result<Value, cad_stdlib::StdLibError> {
    let mut rows = Vec::new();
    for band in ISO_BANDS {
        for nominal in ISO_STEPS_MM {
            rows.push(serde_json::to_value(lookup_iso286(nominal, band)?).unwrap_or(Value::Null));
        }
    }
    Ok(json!({
        "source": "cad-stdlib",
        "nominal_range_mm": [1.0, 250.0],
        "bands": ISO_BANDS,
        "step_edge_mm": ISO_STEPS_MM,
        "deviations": rows,
    }))
}

fn materials() -> Result<Value, cad_stdlib::StdLibError> {
    let mut rows = Vec::new();
    for id in MATERIALS {
        rows.push(serde_json::to_value(lookup_material(id)?).unwrap_or(Value::Null));
    }
    Ok(json!({ "source": "cad-stdlib", "materials": rows }))
}

fn diagnostics() -> Value {
    json!({
        "source": "aicad-diagnostics",
        "entries": [
            play("E102_UNIT_MISMATCH", "A length, angle, or other quantity was mixed.", "Use a Length in mm for size fields. Read the feature example and resend the whole batch."),
            play("E201_AMBIGUOUS_TOPOLOGY", "A topology query matched more than one entity.", "Add a normal, bbox, radius, or tag. Do not take the first hit. Read recovery_hints."),
            play("E409_REVISION_CONFLICT", "expected_revision is stale.", "Call cad_session_context and retry with the new revision. Do not overwrite a newer edit."),
            play("E400_INVALID_COMMAND", "The command shape or catalog key was rejected.", "Read aicad://schema/feature/<kind> and aicad://stdlib/ before retrying."),
            play("E404_NODE_NOT_FOUND", "The command names a missing node or parameter.", "Call cad_inspect_and_slice and use a node id from the outline."),
            play("E422_VALIDATION_FAILED", "The batch compiled but the graph rules failed.", "Read the message, fix the batch, and resend. The document was not changed."),
            play("E_PATH_ESCAPE", "The output path leaves workspace_root.", "Use a relative path inside the workspace. `../` and outside absolute paths are rejected."),
            play("E409_PREVIEW_DIVERGED", "main moved after the preview was forked.", "Diff the preview, then discard or rebase. Do not commit over the newer main."),
            play("E_FILLET_DEGENERATE", "A fillet, chamfer, shell, or boolean could not be built.", "Shrink the radius or thickness. UNKNOWN means the check did not run. Do not replace the solid with a mesh."),
            play("E_EXPERIMENTAL_OPERATOR", "SubD, TPMS, implicit lattice, or FEA was requested.", "Return UNKNOWN. This gateway does not approximate those operators as exact B-Rep.")
        ]
    })
}

fn play(code: &str, summary: &str, recovery: &str) -> Value {
    json!({
        "code": code,
        "summary": summary,
        "recovery": recovery,
    })
}

//! Strongly typed semantic commands.
//!
//! JSON is adjacent-tagged: `{"command":"extrude_sketch","args":{...}}`.
//! The compiler turns these values into [`cad_ir::FeatureSpec`] nodes. It does
//! not splice or parse `.kcl` source.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use cad_ir::{
    AngleUnit, BooleanOpKind, ComponentSourceSpec, DesignAssertionKind, DimInput, DirectFaceEditOp,
    FastenerHoleStandard, FeatureBodyOp, HoleDepthSpec, HoleProfileSpec, IntInput, IrType, IrValue,
    LengthUnit, LinearPatternAxisSpec, MateKindSpec, NodeId, Quantity, SketchConstraint2D,
    SketchEntity2D, SketchPlaneSpec, SketchProfileLoop, SweepPathSpec,
};

use crate::error::SemanticApiError;

fn default_z_axis() -> [f64; 3] {
    [0.0, 0.0, 1.0]
}

fn default_neg_z_axis() -> [f64; 3] {
    [0.0, 0.0, -1.0]
}

const fn default_true() -> bool {
    true
}

/// Engineering contract stored on the document.
///
/// `CanonicalGraph` has no document-level metadata map, so the compiler keeps
/// this value on a reserved design-assertion node. See `walk::CONTRACT_NODE_ID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignContract {
    pub part_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturing_method: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

/// Clearance fit passed to `cad-stdlib` for standard fastener holes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FastenerClearance {
    Close,
    Normal,
    Loose,
}

/// Generic hole geometry, or a catalog fastener hole resolved through `cad-stdlib`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "hole", rename_all = "snake_case")]
pub enum HoleDefinition {
    Generic {
        diameter: DimInput,
        depth: HoleDepthSpec,
        profile: HoleProfileSpec,
    },
    Standard {
        standard: FastenerHoleStandard,
        designation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clearance_class: Option<FastenerClearance>,
        depth: HoleDepthSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_depth: Option<DimInput>,
    },
}

/// Linear or polar pattern of an existing feature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "pattern", rename_all = "snake_case")]
pub enum PatternDefinition {
    Linear {
        direction: [f64; 3],
        count: IntInput,
        spacing: DimInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        second_direction: Option<LinearPatternAxisSpec>,
    },
    Polar {
        #[serde(default)]
        axis_origin_mm: [f64; 3],
        #[serde(default = "default_z_axis")]
        axis_direction: [f64; 3],
        count: IntInput,
        total_angle: DimInput,
    },
}

/// Sweep path: another sketch, or an explicit polyline in millimetres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "path", rename_all = "snake_case")]
pub enum SweepPathInput {
    Sketch { node_id: NodeId },
    Polyline { points_mm: Vec<[f64; 3]> },
}

impl SweepPathInput {
    pub(crate) fn to_spec(&self) -> SweepPathSpec {
        match self {
            Self::Sketch { node_id } => SweepPathSpec::Sketch {
                node_id: node_id.clone(),
            },
            Self::Polyline { points_mm } => SweepPathSpec::Polyline {
                points_mm: points_mm.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertParameter {
    pub name: String,
    pub ir_type: IrType,
    pub value: IrValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
}

/// Change one literal input on an existing feature without deleting the node.
///
/// A parameter-bound or node-bound input is rejected. Change that binding with
/// `upsert_parameter` instead. `length_mm` is for `width`, `depth`, `height`,
/// or `diameter` on `primitive_box` / `hole`. `vector_mm` is for `origin_mm`
/// or `axis`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetFeatureInput {
    pub node_id: NodeId,
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_mm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_mm: Option<[f64; 3]>,
}

impl UpsertParameter {
    pub fn length_mm(
        name: impl Into<String>,
        millimeters: f64,
        formula: Option<String>,
    ) -> Result<Self, SemanticApiError> {
        let quantity =
            Quantity::length(millimeters, LengthUnit::Mm).map_err(SemanticApiError::from_ir)?;
        Ok(Self {
            name: name.into(),
            ir_type: IrType::Length,
            value: IrValue::Quantity(quantity),
            formula,
        })
    }

    pub fn angle_deg(
        name: impl Into<String>,
        degrees: f64,
        formula: Option<String>,
    ) -> Result<Self, SemanticApiError> {
        let quantity =
            Quantity::angle(degrees, AngleUnit::Deg).map_err(SemanticApiError::from_ir)?;
        Ok(Self {
            name: name.into(),
            ir_type: IrType::Angle,
            value: IrValue::Quantity(quantity),
            formula,
        })
    }

    pub fn integer(name: impl Into<String>, value: i64, formula: Option<String>) -> Self {
        Self {
            name: name.into(),
            ir_type: IrType::Int,
            value: IrValue::Int(value),
            formula,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveParameter {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetDesignContract {
    pub contract: DesignContract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSketch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub plane: SketchPlaneSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<SketchEntity2D>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<SketchProfileLoop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<SketchConstraint2D>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertSketchEntities {
    pub node_id: NodeId,
    pub entities: Vec<SketchEntity2D>,
    #[serde(default)]
    pub replace: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertSketchConstraints {
    pub node_id: NodeId,
    pub constraints: Vec<SketchConstraint2D>,
    #[serde(default)]
    pub replace: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePrimitiveBox {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub width: DimInput,
    pub depth: DimInput,
    pub height: DimInput,
    #[serde(default)]
    pub origin_mm: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePrimitiveCylinder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub radius: DimInput,
    pub height: DimInput,
    #[serde(default)]
    pub origin_mm: [f64; 3],
    #[serde(default = "default_z_axis")]
    pub axis: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtrudeSketch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub sketch: NodeId,
    pub distance: DimInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f64; 3]>,
    #[serde(default)]
    pub symmetric: bool,
    #[serde(default)]
    pub operation: FeatureBodyOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevolveSketch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub sketch: NodeId,
    #[serde(default)]
    pub axis_origin_mm: [f64; 3],
    #[serde(default = "default_z_axis")]
    pub axis_direction: [f64; 3],
    pub angle: DimInput,
    #[serde(default)]
    pub operation: FeatureBodyOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub profile: NodeId,
    pub path: SweepPathInput,
    #[serde(default)]
    pub keep_normal: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoftSections {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub sections: Vec<NodeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<NodeId>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub as_surface: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateHole {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_face: Option<String>,
    #[serde(default)]
    pub origin_mm: [f64; 3],
    #[serde(default = "default_neg_z_axis")]
    pub axis: [f64; 3],
    pub definition: HoleDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilletEdges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<String>,
    pub radius: DimInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChamferEdges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<String>,
    pub distance: DimInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance2: Option<DimInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<DimInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellSolid {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_faces: Vec<String>,
    pub thickness: DimInput,
    #[serde(default = "default_true")]
    pub inward: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftFaces {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    pub faces: Vec<String>,
    pub pull_direction: [f64; 3],
    pub neutral_plane: SketchPlaneSpec,
    pub angle: DimInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternFeature {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    pub definition: PatternDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorFeature {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    pub plane: SketchPlaneSpec,
    #[serde(default = "default_true")]
    pub keep_original: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BooleanSolids {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    // Aliases catch common LLM/schema guesses from TES-1-P1-C4 without
    // loosening deny_unknown_fields for unrelated keys.
    #[serde(alias = "operation")]
    pub op: BooleanOpKind,
    #[serde(alias = "base")]
    pub target: NodeId,
    #[serde(alias = "solids")]
    pub tools: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectFaceEditCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    pub face: String,
    pub edit: DirectFaceEditOp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsertComponent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub source: ComponentSourceSpec,
    #[serde(default)]
    pub translation_mm: [f64; 3],
    #[serde(default)]
    pub rotation_deg: [f64; 3],
    #[serde(default)]
    pub grounded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddMate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub left: NodeId,
    pub right: NodeId,
    pub left_ref: String,
    pub right_ref: String,
    pub mate_kind: MateKindSpec,
    #[serde(default)]
    pub flip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameNode {
    pub node_id: NodeId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetNodeSuppressed {
    pub node_id: NodeId,
    pub suppressed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteNode {
    pub node_id: NodeId,
}

/// Move `node_id` immediately before `before`. `None` moves it to the end of the feature tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReorderFeature {
    pub node_id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagTopologyCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    pub solid: NodeId,
    pub tag: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddDesignAssertion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<NodeId>,
    pub assertion_id: String,
    pub kind: DesignAssertionKind,
}

/// One high-level CAD intent. A batch of these compiles into a single [`cad_ir::GraphPatch`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "command",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SemanticCommand {
    UpsertParameter(UpsertParameter),
    SetFeatureInput(SetFeatureInput),
    RemoveParameter(RemoveParameter),
    SetDesignContract(SetDesignContract),
    CreateSketch(CreateSketch),
    UpsertSketchEntities(UpsertSketchEntities),
    UpsertSketchConstraints(UpsertSketchConstraints),
    CreatePrimitiveBox(CreatePrimitiveBox),
    CreatePrimitiveCylinder(CreatePrimitiveCylinder),
    ExtrudeSketch(ExtrudeSketch),
    RevolveSketch(RevolveSketch),
    SweepProfile(SweepProfile),
    LoftSections(LoftSections),
    CreateHole(CreateHole),
    FilletEdges(FilletEdges),
    ChamferEdges(ChamferEdges),
    ShellSolid(ShellSolid),
    DraftFaces(DraftFaces),
    PatternFeature(PatternFeature),
    MirrorFeature(MirrorFeature),
    BooleanSolids(BooleanSolids),
    DirectFaceEdit(DirectFaceEditCommand),
    InsertComponent(InsertComponent),
    AddMate(AddMate),
    RenameNode(RenameNode),
    SetNodeSuppressed(SetNodeSuppressed),
    DeleteNode(DeleteNode),
    ReorderFeature(ReorderFeature),
    TagTopology(TagTopologyCommand),
    AddDesignAssertion(AddDesignAssertion),
}

impl SemanticCommand {
    pub fn command_name(&self) -> &'static str {
        match self {
            Self::UpsertParameter(_) => "upsert_parameter",
            Self::SetFeatureInput(_) => "set_feature_input",
            Self::RemoveParameter(_) => "remove_parameter",
            Self::SetDesignContract(_) => "set_design_contract",
            Self::CreateSketch(_) => "create_sketch",
            Self::UpsertSketchEntities(_) => "upsert_sketch_entities",
            Self::UpsertSketchConstraints(_) => "upsert_sketch_constraints",
            Self::CreatePrimitiveBox(_) => "create_primitive_box",
            Self::CreatePrimitiveCylinder(_) => "create_primitive_cylinder",
            Self::ExtrudeSketch(_) => "extrude_sketch",
            Self::RevolveSketch(_) => "revolve_sketch",
            Self::SweepProfile(_) => "sweep_profile",
            Self::LoftSections(_) => "loft_sections",
            Self::CreateHole(_) => "create_hole",
            Self::FilletEdges(_) => "fillet_edges",
            Self::ChamferEdges(_) => "chamfer_edges",
            Self::ShellSolid(_) => "shell_solid",
            Self::DraftFaces(_) => "draft_faces",
            Self::PatternFeature(_) => "pattern_feature",
            Self::MirrorFeature(_) => "mirror_feature",
            Self::BooleanSolids(_) => "boolean_solids",
            Self::DirectFaceEdit(_) => "direct_face_edit",
            Self::InsertComponent(_) => "insert_component",
            Self::AddMate(_) => "add_mate",
            Self::RenameNode(_) => "rename_node",
            Self::SetNodeSuppressed(_) => "set_node_suppressed",
            Self::DeleteNode(_) => "delete_node",
            Self::ReorderFeature(_) => "reorder_feature",
            Self::TagTopology(_) => "tag_topology",
            Self::AddDesignAssertion(_) => "add_design_assertion",
        }
    }
}

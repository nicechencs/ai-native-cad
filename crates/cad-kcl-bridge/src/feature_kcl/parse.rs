//! `.kcl` feature block -> [`cad_ir::FeatureSpec`].

use std::collections::BTreeMap;

use cad_ir::{
    BlobHandle, BooleanOpKind, ComponentSourceSpec, DatumPlane, DesignAssertionKind, DimInput,
    DirectFaceEditOp, ExternalRefId, FastenerHoleStandard, FeatureBodyOp, FeatureSpec,
    HoleDepthSpec, HoleProfileSpec, IntInput, LinearPatternAxisSpec, MateKindSpec, NodeId,
    Quantity, SketchAnchor, SketchConstraint2D, SketchDimValue, SketchEntity2D, SketchPlaneSpec,
    SketchPointRef, SketchProfileLoop, StructuredSketch2D, SweepPathSpec, OUTPUT_PORT,
};
use cad_syntax::{Token, TokenKind};

use crate::model::{blob_handle_from_keyword, quantity_from_suffix, SuffixParse};
use crate::BridgeError;

const ZERO: [f64; 3] = [0.0, 0.0, 0.0];
const Z_AXIS: [f64; 3] = [0.0, 0.0, 1.0];
const NEG_Z: [f64; 3] = [0.0, 0.0, -1.0];

pub(crate) struct ParsedFeature {
    pub spec: FeatureSpec,
    pub cursor: usize,
    pub node_refs: Vec<(String, usize, usize)>,
    pub param_uses: Vec<(String, usize, usize)>,
    pub blob_uses: Vec<(String, usize, usize)>,
    pub extref_uses: Vec<(String, usize, usize)>,
}

pub(crate) fn parse_feature_block(
    tokens: &[Token],
    cursor: usize,
    op: &str,
    op_at: (usize, usize),
) -> Result<ParsedFeature, BridgeError> {
    let mut cur = Cur {
        tokens,
        cursor,
        node_refs: Vec::new(),
        param_uses: Vec::new(),
        blob_uses: Vec::new(),
        extref_uses: Vec::new(),
    };
    let block_at = cur.location();
    let fields = cur.parse_block()?;
    let spec = lower_feature(&mut cur, op, op_at, block_at, &fields)?;
    Ok(ParsedFeature {
        spec,
        cursor: cur.cursor,
        node_refs: cur.node_refs,
        param_uses: cur.param_uses,
        blob_uses: cur.blob_uses,
        extref_uses: cur.extref_uses,
    })
}

struct Cur<'a> {
    tokens: &'a [Token],
    cursor: usize,
    node_refs: Vec<(String, usize, usize)>,
    param_uses: Vec<(String, usize, usize)>,
    blob_uses: Vec<(String, usize, usize)>,
    extref_uses: Vec<(String, usize, usize)>,
}

struct Fx {
    at: (usize, usize),
    kind: FxKind,
}

enum FxKind {
    Ident(String),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Quantity(Quantity),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Topo(String),
    Blob(String),
    Ext(String),
    Param(String),
    Node {
        id: String,
        port: String,
    },
    List(Vec<Fx>),
    Map(Vec<(String, Fx)>),
    Call {
        name: String,
        positional: Vec<Fx>,
        named: Vec<(String, Fx)>,
    },
}

struct Call<'a> {
    at: (usize, usize),
    name: &'a str,
    positional: &'a [Fx],
    named: &'a [(String, Fx)],
}

fn lower_feature(
    cur: &mut Cur<'_>,
    op: &str,
    op_at: (usize, usize),
    block_at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    match op {
        "box" => lower_box(cur, block_at, fields),
        "cylinder" => lower_cylinder(cur, block_at, fields),
        "sketch_2d" => lower_sketch(cur, block_at, fields),
        "extrude" => lower_extrude(cur, block_at, fields),
        "revolve" => lower_revolve(cur, block_at, fields),
        "sweep" => lower_sweep(cur, block_at, fields),
        "loft_guided" => lower_loft(cur, block_at, fields),
        "hole" => lower_hole(cur, block_at, fields),
        "std_fastener_hole" => lower_fastener(cur, block_at, fields),
        "fillet" => lower_fillet(cur, block_at, fields),
        "chamfer" => lower_chamfer(cur, block_at, fields),
        "shell" => lower_shell(cur, block_at, fields),
        "draft" => lower_draft(cur, block_at, fields),
        "pattern_linear" => lower_linear(cur, block_at, fields),
        "pattern_polar" => lower_polar(cur, block_at, fields),
        "mirror" => lower_mirror(cur, block_at, fields),
        "boolean_union" => lower_boolean(cur, block_at, fields, BooleanOpKind::Union),
        "boolean_subtract" => lower_boolean(cur, block_at, fields, BooleanOpKind::Subtract),
        "boolean_intersect" => lower_boolean(cur, block_at, fields, BooleanOpKind::Intersect),
        "direct_face_edit" => lower_direct(cur, block_at, fields),
        "import::asset" => lower_import(cur, block_at, fields),
        "assembly::component" => lower_component(cur, block_at, fields),
        "assembly::mate" => lower_mate(cur, block_at, fields),
        "tag::faces" => lower_tag(cur, block_at, fields),
        "assert::gate" => lower_assert(cur, block_at, fields),
        _ => Err(BridgeError::at(
            op_at.0,
            op_at.1,
            format!("`{op}` is not a feature block"),
        )),
    }
}

fn lower_box(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["width", "depth", "height", "origin"])?;
    Ok(FeatureSpec::PrimitiveBox {
        width: cur.as_dim(required(fields, "width", at)?)?,
        depth: cur.as_dim(required(fields, "depth", at)?)?,
        height: cur.as_dim(required(fields, "height", at)?)?,
        origin_mm: optional_vec3(fields, "origin")?.unwrap_or(ZERO),
    })
}

fn lower_cylinder(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["radius", "height", "origin", "axis"])?;
    Ok(FeatureSpec::PrimitiveCylinder {
        radius: cur.as_dim(required(fields, "radius", at)?)?,
        height: cur.as_dim(required(fields, "height", at)?)?,
        origin_mm: optional_vec3(fields, "origin")?.unwrap_or(ZERO),
        axis: optional_vec3(fields, "axis")?.unwrap_or(Z_AXIS),
    })
}

fn lower_sketch(
    _cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &["plane", "entities", "profiles", "constraints", "dof"],
    )?;
    let plane = as_plane(required(fields, "plane", at)?)?;
    let entities = match fields.get("entities") {
        None => Vec::new(),
        Some(fx) => as_list(fx)?
            .iter()
            .map(as_entity)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let profiles = match fields.get("profiles") {
        None => Vec::new(),
        Some(fx) => as_list(fx)?
            .iter()
            .map(as_profile)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let constraints = match fields.get("constraints") {
        None => Vec::new(),
        Some(fx) => as_list(fx)?
            .iter()
            .map(as_constraint)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let remaining_dof = match fields.get("dof") {
        None => None,
        Some(fx) => Some(as_u32(fx)?),
    };
    Ok(FeatureSpec::Sketch2D(StructuredSketch2D {
        plane,
        entities,
        profiles,
        constraints,
        remaining_dof,
    }))
}

fn lower_extrude(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &[
            "sketch",
            "distance",
            "direction",
            "symmetric",
            "operation",
            "target",
        ],
    )?;
    Ok(FeatureSpec::Extrude {
        sketch: cur.as_node_id(required(fields, "sketch", at)?)?,
        distance: cur.as_dim(required(fields, "distance", at)?)?,
        direction: optional_vec3(fields, "direction")?,
        symmetric: optional_bool(fields, "symmetric")?.unwrap_or(false),
        operation: optional_body_op(fields)?.unwrap_or(FeatureBodyOp::NewBody),
        target: optional_node(cur, fields, "target")?,
    })
}

fn lower_revolve(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &["sketch", "origin", "axis", "angle", "operation", "target"],
    )?;
    Ok(FeatureSpec::Revolve {
        sketch: cur.as_node_id(required(fields, "sketch", at)?)?,
        axis_origin_mm: optional_vec3(fields, "origin")?.unwrap_or(ZERO),
        axis_direction: optional_vec3(fields, "axis")?.unwrap_or(Z_AXIS),
        angle: cur.as_dim(required(fields, "angle", at)?)?,
        operation: optional_body_op(fields)?.unwrap_or(FeatureBodyOp::NewBody),
        target: optional_node(cur, fields, "target")?,
    })
}

fn lower_sweep(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["profile", "path", "keep_normal"])?;
    let path_fx = required(fields, "path", at)?;
    let path = if let FxKind::List(items) = &path_fx.kind {
        let mut points = Vec::with_capacity(items.len());
        for item in items {
            points.push(as_vec3(item)?);
        }
        SweepPathSpec::Polyline { points_mm: points }
    } else {
        SweepPathSpec::Sketch {
            node_id: cur.as_node_id(path_fx)?,
        }
    };
    Ok(FeatureSpec::Sweep {
        profile: cur.as_node_id(required(fields, "profile", at)?)?,
        path,
        keep_normal: optional_bool(fields, "keep_normal")?.unwrap_or(false),
    })
}

fn lower_loft(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["sections", "guides", "closed", "as_surface"])?;
    Ok(FeatureSpec::Loft {
        sections: cur.as_node_list(required(fields, "sections", at)?)?,
        guides: match fields.get("guides") {
            None => Vec::new(),
            Some(fx) => cur.as_node_list(fx)?,
        },
        closed: optional_bool(fields, "closed")?.unwrap_or(false),
        as_surface: optional_bool(fields, "as_surface")?.unwrap_or(false),
    })
}

fn lower_hole(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &[
            "solid", "face", "origin", "axis", "diameter", "depth", "profile",
        ],
    )?;
    Ok(FeatureSpec::Hole {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        target_face: optional_topo(fields, "face")?,
        origin_mm: optional_vec3(fields, "origin")?.unwrap_or(ZERO),
        axis: optional_vec3(fields, "axis")?.unwrap_or(NEG_Z),
        diameter: cur.as_dim(required(fields, "diameter", at)?)?,
        depth: match fields.get("depth") {
            None => HoleDepthSpec::ThroughAll,
            Some(fx) => as_depth(cur, fx)?,
        },
        profile: match fields.get("profile") {
            None => HoleProfileSpec::Simple,
            Some(fx) => as_hole_profile(cur, fx)?,
        },
    })
}

fn lower_fastener(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &[
            "solid",
            "face",
            "origin",
            "axis",
            "standard",
            "designation",
            "clearance",
            "depth",
            "thread_depth",
        ],
    )?;
    let standard_fx = required(fields, "standard", at)?;
    let standard_name = as_text(standard_fx)?;
    let standard = fastener_from(&standard_name).ok_or_else(|| {
        fail(
            standard_fx,
            format!("unknown fastener standard `{standard_name}`"),
        )
    })?;
    Ok(FeatureSpec::StdFastenerHole {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        target_face: optional_topo(fields, "face")?,
        origin_mm: optional_vec3(fields, "origin")?.unwrap_or(ZERO),
        axis: optional_vec3(fields, "axis")?.unwrap_or(NEG_Z),
        standard,
        designation: as_text(required(fields, "designation", at)?)?,
        clearance_class: fields.get("clearance").map(as_text).transpose()?,
        depth: match fields.get("depth") {
            None => HoleDepthSpec::ThroughAll,
            Some(fx) => as_depth(cur, fx)?,
        },
        thread_depth: match fields.get("thread_depth") {
            None => None,
            Some(fx) => Some(cur.as_dim(fx)?),
        },
    })
}

fn lower_fillet(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["solid", "edges", "radius"])?;
    Ok(FeatureSpec::Fillet {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        edges: optional_topo_list(fields, "edges")?,
        radius: cur.as_dim(required(fields, "radius", at)?)?,
    })
}

fn lower_chamfer(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &["solid", "edges", "distance", "distance2", "angle"],
    )?;
    Ok(FeatureSpec::Chamfer {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        edges: optional_topo_list(fields, "edges")?,
        distance: cur.as_dim(required(fields, "distance", at)?)?,
        distance2: optional_dim(cur, fields, "distance2")?,
        angle: optional_dim(cur, fields, "angle")?,
    })
}

fn lower_shell(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["solid", "open_faces", "thickness", "inward"])?;
    Ok(FeatureSpec::Shell {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        open_faces: optional_topo_list(fields, "open_faces")?,
        thickness: cur.as_dim(required(fields, "thickness", at)?)?,
        inward: optional_bool(fields, "inward")?.unwrap_or(true),
    })
}

fn lower_draft(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &["solid", "faces", "pull_direction", "neutral_plane", "angle"],
    )?;
    Ok(FeatureSpec::Draft {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        faces: optional_topo_list(fields, "faces")?,
        pull_direction: as_vec3(required(fields, "pull_direction", at)?)?,
        neutral_plane: as_plane(required(fields, "neutral_plane", at)?)?,
        angle: cur.as_dim(required(fields, "angle", at)?)?,
    })
}

fn lower_linear(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &["solid", "direction", "count", "spacing", "second"],
    )?;
    let second_direction = match fields.get("second") {
        None => None,
        Some(fx) => Some(as_pattern_axis(cur, fx)?),
    };
    Ok(FeatureSpec::LinearPattern {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        direction: as_vec3(required(fields, "direction", at)?)?,
        count: cur.as_int(required(fields, "count", at)?)?,
        spacing: cur.as_dim(required(fields, "spacing", at)?)?,
        second_direction,
    })
}

fn lower_polar(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &[
            "solid",
            "axis_origin",
            "axis",
            "count",
            "angle",
            "radius",
            "start_angle",
        ],
    )?;
    Ok(FeatureSpec::PolarPattern {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        axis_origin_mm: optional_vec3(fields, "axis_origin")?.unwrap_or(ZERO),
        axis_direction: optional_vec3(fields, "axis")?.unwrap_or(Z_AXIS),
        count: cur.as_int(required(fields, "count", at)?)?,
        total_angle: cur.as_dim(required(fields, "angle", at)?)?,
        radius: optional_dim(cur, fields, "radius")?,
        start_angle: optional_dim(cur, fields, "start_angle")?,
    })
}

fn lower_mirror(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["solid", "plane", "keep_original"])?;
    Ok(FeatureSpec::Mirror {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        plane: as_plane(required(fields, "plane", at)?)?,
        keep_original: optional_bool(fields, "keep_original")?.unwrap_or(true),
    })
}

fn lower_boolean(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
    op: BooleanOpKind,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["target", "tools"])?;
    Ok(FeatureSpec::Boolean {
        op,
        target: cur.as_node_id(required(fields, "target", at)?)?,
        tools: cur.as_node_list(required(fields, "tools", at)?)?,
    })
}

fn lower_direct(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["solid", "face", "offset", "move", "delete"])?;
    let edit = match (
        fields.get("offset"),
        fields.get("move"),
        fields.get("delete"),
    ) {
        (Some(offset), None, None) => DirectFaceEditOp::Offset {
            distance: cur.as_dim(offset)?,
        },
        (None, Some(delta), None) => DirectFaceEditOp::Move {
            delta_mm: as_vec3(delta)?,
        },
        (None, None, Some(flag)) => {
            if !as_bool(flag)? {
                return Err(fail(flag, "`delete` must be true"));
            }
            DirectFaceEditOp::DeleteAndHeal
        }
        _ => {
            return Err(BridgeError::at(
                at.0,
                at.1,
                "direct face edit needs exactly one of `offset`, `move`, or `delete`",
            ));
        }
    };
    Ok(FeatureSpec::DirectFaceEdit {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        face: as_topo(required(fields, "face", at)?)?,
        edit,
    })
}

fn lower_import(
    _cur: &mut Cur<'_>,
    _at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["blob", "path", "scale", "reference_only"])?;
    Ok(FeatureSpec::ImportStep {
        blob: match fields.get("blob") {
            None => None,
            Some(fx) => Some(BlobHandle::from(as_blob(fx)?)),
        },
        path: match fields.get("path") {
            None => None,
            Some(fx) => Some(as_string(fx)?),
        },
        scale: match fields.get("scale") {
            None => 1.0,
            Some(fx) => as_f64(fx)?,
        },
        as_reference_only: optional_bool(fields, "reference_only")?.unwrap_or(false),
    })
}

fn lower_component(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &[
            "geometry",
            "external_ref",
            "translation",
            "rotation",
            "grounded",
        ],
    )?;
    let source = match (fields.get("geometry"), fields.get("external_ref")) {
        (Some(geometry), None) => ComponentSourceSpec::LocalNode {
            node_id: cur.as_node_id(geometry)?,
        },
        (None, Some(external)) => ComponentSourceSpec::ExternalRef {
            ref_id: ExternalRefId::from(as_ext(external)?),
        },
        _ => {
            return Err(BridgeError::at(
                at.0,
                at.1,
                "component needs exactly one of `geometry` or `external_ref`",
            ));
        }
    };
    Ok(FeatureSpec::ComponentInstance {
        source,
        translation_mm: optional_vec3(fields, "translation")?.unwrap_or(ZERO),
        rotation_deg: optional_vec3(fields, "rotation")?.unwrap_or(ZERO),
        grounded: optional_bool(fields, "grounded")?.unwrap_or(false),
    })
}

fn lower_mate(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(
        fields,
        &["left", "right", "left_ref", "right_ref", "kind", "flip"],
    )?;
    Ok(FeatureSpec::MateConstraint {
        left: cur.as_node_id(required(fields, "left", at)?)?,
        right: cur.as_node_id(required(fields, "right", at)?)?,
        left_ref: as_topo(required(fields, "left_ref", at)?)?,
        right_ref: as_topo(required(fields, "right_ref", at)?)?,
        mate_kind: as_mate_kind(cur, required(fields, "kind", at)?)?,
        flip: optional_bool(fields, "flip")?.unwrap_or(false),
    })
}

fn lower_tag(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["solid", "tag", "targets"])?;
    Ok(FeatureSpec::TagTopology {
        solid: cur.as_node_id(required(fields, "solid", at)?)?,
        tag: as_text(required(fields, "tag", at)?)?,
        targets: optional_topo_list(fields, "targets")?,
    })
}

fn lower_assert(
    cur: &mut Cur<'_>,
    at: (usize, usize),
    fields: &BTreeMap<String, Fx>,
) -> Result<FeatureSpec, BridgeError> {
    allow_keys(fields, &["target", "id", "rule"])?;
    Ok(FeatureSpec::DesignAssertion {
        target: optional_node(cur, fields, "target")?,
        assertion_id: as_text(required(fields, "id", at)?)?,
        kind: as_rule(cur, required(fields, "rule", at)?)?,
    })
}

fn optional_vec3(
    fields: &BTreeMap<String, Fx>,
    key: &str,
) -> Result<Option<[f64; 3]>, BridgeError> {
    fields.get(key).map(as_vec3).transpose()
}

fn optional_bool(fields: &BTreeMap<String, Fx>, key: &str) -> Result<Option<bool>, BridgeError> {
    fields.get(key).map(as_bool).transpose()
}

fn optional_dim(
    cur: &mut Cur<'_>,
    fields: &BTreeMap<String, Fx>,
    key: &str,
) -> Result<Option<DimInput>, BridgeError> {
    match fields.get(key) {
        None => Ok(None),
        Some(fx) => Ok(Some(cur.as_dim(fx)?)),
    }
}

fn optional_node(
    cur: &mut Cur<'_>,
    fields: &BTreeMap<String, Fx>,
    key: &str,
) -> Result<Option<NodeId>, BridgeError> {
    match fields.get(key) {
        None => Ok(None),
        Some(fx) => Ok(Some(cur.as_node_id(fx)?)),
    }
}

fn optional_topo(fields: &BTreeMap<String, Fx>, key: &str) -> Result<Option<String>, BridgeError> {
    fields.get(key).map(as_topo).transpose()
}

fn optional_topo_list(
    fields: &BTreeMap<String, Fx>,
    key: &str,
) -> Result<Vec<String>, BridgeError> {
    match fields.get(key) {
        None => Ok(Vec::new()),
        Some(fx) => as_list(fx)?
            .iter()
            .map(as_topo)
            .collect::<Result<Vec<_>, _>>(),
    }
}

fn optional_body_op(fields: &BTreeMap<String, Fx>) -> Result<Option<FeatureBodyOp>, BridgeError> {
    let Some(fx) = fields.get("operation") else {
        return Ok(None);
    };
    let name = as_text(fx)?;
    body_op_from(&name)
        .map(Some)
        .ok_or_else(|| fail(fx, format!("unknown body operation `{name}`")))
}

fn as_depth(cur: &mut Cur<'_>, fx: &Fx) -> Result<HoleDepthSpec, BridgeError> {
    match &fx.kind {
        FxKind::Ident(name) if name == "through_all" => Ok(HoleDepthSpec::ThroughAll),
        FxKind::Call { .. } => {
            let call = as_call(fx)?;
            if call.name != "blind" {
                return Err(fail(fx, "depth must be `through_all` or `blind(...)`"));
            }
            allow_named(&call, &[])?;
            let dim = one_positional(&call)?;
            Ok(HoleDepthSpec::Blind {
                depth: cur.as_dim(dim)?,
            })
        }
        _ => Err(fail(fx, "depth must be `through_all` or `blind(...)`")),
    }
}

fn as_hole_profile(cur: &mut Cur<'_>, fx: &Fx) -> Result<HoleProfileSpec, BridgeError> {
    match &fx.kind {
        FxKind::Ident(name) if name == "simple" => Ok(HoleProfileSpec::Simple),
        FxKind::Call { .. } => {
            let call = as_call(fx)?;
            match call.name {
                "counterbore" => {
                    allow_named(&call, &["diameter", "depth"])?;
                    expect_positional(&call, 0)?;
                    Ok(HoleProfileSpec::Counterbore {
                        bore_diameter: cur.as_dim(must_named(&call, "diameter")?)?,
                        bore_depth: cur.as_dim(must_named(&call, "depth")?)?,
                    })
                }
                "countersink" => {
                    allow_named(&call, &["diameter", "angle"])?;
                    expect_positional(&call, 0)?;
                    Ok(HoleProfileSpec::Countersink {
                        sink_diameter: cur.as_dim(must_named(&call, "diameter")?)?,
                        sink_angle: cur.as_dim(must_named(&call, "angle")?)?,
                    })
                }
                _ => Err(fail(
                    fx,
                    "profile must be `simple`, `counterbore(...)`, or `countersink(...)`",
                )),
            }
        }
        _ => Err(fail(
            fx,
            "profile must be `simple`, `counterbore(...)`, or `countersink(...)`",
        )),
    }
}

fn as_pattern_axis(cur: &mut Cur<'_>, fx: &Fx) -> Result<LinearPatternAxisSpec, BridgeError> {
    let call = as_call(fx)?;
    if call.name != "axis" {
        return Err(fail(fx, "expected `axis(...)`"));
    }
    allow_named(&call, &["direction", "count", "spacing"])?;
    expect_positional(&call, 0)?;
    Ok(LinearPatternAxisSpec {
        direction: as_vec3(must_named(&call, "direction")?)?,
        count: cur.as_int(must_named(&call, "count")?)?,
        spacing: cur.as_dim(must_named(&call, "spacing")?)?,
    })
}

fn as_mate_kind(cur: &mut Cur<'_>, fx: &Fx) -> Result<MateKindSpec, BridgeError> {
    match &fx.kind {
        FxKind::Ident(name) => match name.as_str() {
            "coincident" => Ok(MateKindSpec::Coincident),
            "concentric" => Ok(MateKindSpec::Concentric),
            "parallel" => Ok(MateKindSpec::Parallel),
            "perpendicular" => Ok(MateKindSpec::Perpendicular),
            _ => Err(fail(fx, format!("unknown mate kind `{name}`"))),
        },
        FxKind::Call { .. } => {
            let call = as_call(fx)?;
            allow_named(&call, &[])?;
            let arg = one_positional(&call)?;
            match call.name {
                "distance" => Ok(MateKindSpec::Distance {
                    value: cur.as_dim(arg)?,
                }),
                "angle" => Ok(MateKindSpec::Angle {
                    value: cur.as_dim(arg)?,
                }),
                "gear" => Ok(MateKindSpec::Gear {
                    ratio: as_f64(arg)?,
                }),
                _ => Err(fail(fx, format!("unknown mate kind `{}`", call.name))),
            }
        }
        _ => Err(fail(fx, "expected a mate kind")),
    }
}

fn as_rule(cur: &mut Cur<'_>, fx: &Fx) -> Result<DesignAssertionKind, BridgeError> {
    let call = as_call(fx)?;
    match call.name {
        "volume" => {
            allow_named(&call, &["min", "max"])?;
            expect_positional(&call, 0)?;
            Ok(DesignAssertionKind::VolumeBounds {
                min_mm3: match named(&call, "min") {
                    None => None,
                    Some(fx) => Some(as_f64(fx)?),
                },
                max_mm3: match named(&call, "max") {
                    None => None,
                    Some(fx) => Some(as_f64(fx)?),
                },
            })
        }
        "min_wall" => {
            allow_named(&call, &[])?;
            Ok(DesignAssertionKind::MinWallThickness {
                min: cur.as_dim(one_positional(&call)?)?,
            })
        }
        "condition" => {
            allow_named(&call, &[])?;
            Ok(DesignAssertionKind::BooleanCondition {
                condition: cur.as_node_id(one_positional(&call)?)?,
            })
        }
        "custom" => {
            allow_named(&call, &["params"])?;
            let expression = as_string(one_positional(&call)?)?;
            let params = match named(&call, "params") {
                None => BTreeMap::new(),
                Some(fx) => as_string_map(fx)?,
            };
            Ok(DesignAssertionKind::Custom { expression, params })
        }
        _ => Err(fail(
            fx,
            "rule must be `volume`, `min_wall`, `condition`, or `custom`",
        )),
    }
}

fn as_plane(fx: &Fx) -> Result<SketchPlaneSpec, BridgeError> {
    let call = as_call(fx)?;
    match call.name {
        "datum" => {
            allow_named(&call, &["offset"])?;
            let plane = datum_from(&as_text(one_positional(&call)?)?).ok_or_else(|| {
                fail(
                    &call.positional[0],
                    "datum plane must be `xy`, `xz`, or `yz`",
                )
            })?;
            let offset = match named(&call, "offset") {
                None => None,
                Some(fx) => Some(as_sketch_dim(fx)?),
            };
            Ok(SketchPlaneSpec::Datum { plane, offset })
        }
        "face" => {
            allow_named(&call, &["offset"])?;
            let face = as_topo(one_positional(&call)?)?;
            let offset = match named(&call, "offset") {
                None => None,
                Some(fx) => Some(as_sketch_dim(fx)?),
            };
            Ok(SketchPlaneSpec::FaceRef { face, offset })
        }
        "custom" => {
            allow_named(&call, &["origin", "normal", "x_axis"])?;
            expect_positional(&call, 0)?;
            Ok(SketchPlaneSpec::Custom {
                origin_mm: as_vec3(must_named(&call, "origin")?)?,
                normal: as_vec3(must_named(&call, "normal")?)?,
                x_axis: match named(&call, "x_axis") {
                    None => None,
                    Some(fx) => Some(as_vec3(fx)?),
                },
            })
        }
        _ => Err(fail(fx, "expected `datum`, `face`, or `custom` plane")),
    }
}

fn as_entity(fx: &Fx) -> Result<SketchEntity2D, BridgeError> {
    let call = as_call(fx)?;
    let id = as_text(one_positional(&call)?)?;
    match call.name {
        "point" => {
            allow_named(&call, &["at", "fixed", "construction"])?;
            Ok(SketchEntity2D::Point {
                id,
                position_mm: as_vec2(must_named(&call, "at")?)?,
                fixed: flag(&call, "fixed")?,
                construction: flag(&call, "construction")?,
            })
        }
        "line" => {
            allow_named(&call, &["start", "end", "construction"])?;
            Ok(SketchEntity2D::Line {
                id,
                start_mm: as_vec2(must_named(&call, "start")?)?,
                end_mm: as_vec2(must_named(&call, "end")?)?,
                construction: flag(&call, "construction")?,
            })
        }
        "arc" => {
            allow_named(&call, &["center", "start", "end", "ccw", "construction"])?;
            Ok(SketchEntity2D::Arc {
                id,
                center_mm: as_vec2(must_named(&call, "center")?)?,
                start_mm: as_vec2(must_named(&call, "start")?)?,
                end_mm: as_vec2(must_named(&call, "end")?)?,
                ccw: flag(&call, "ccw")?,
                construction: flag(&call, "construction")?,
            })
        }
        "circle" => {
            allow_named(&call, &["center", "radius", "construction"])?;
            Ok(SketchEntity2D::Circle {
                id,
                center_mm: as_vec2(must_named(&call, "center")?)?,
                radius_mm: as_f64(must_named(&call, "radius")?)?,
                construction: flag(&call, "construction")?,
            })
        }
        "rectangle" => {
            allow_named(
                &call,
                &["origin", "width", "height", "centered", "construction"],
            )?;
            Ok(SketchEntity2D::Rectangle {
                id,
                origin_mm: as_vec2(must_named(&call, "origin")?)?,
                width_mm: as_f64(must_named(&call, "width")?)?,
                height_mm: as_f64(must_named(&call, "height")?)?,
                centered: flag(&call, "centered")?,
                construction: flag(&call, "construction")?,
            })
        }
        "spline" => {
            if named(&call, "points").is_some() {
                allow_named(&call, &["points", "segments", "closed", "construction"])?;
                let pts = as_list(must_named(&call, "points")?)?
                    .iter()
                    .map(as_vec2)
                    .collect::<Result<Vec<_>, _>>()?;
                let segments_per_span = match named(&call, "segments") {
                    None => None,
                    Some(fx) => Some(as_u32(fx)?),
                };
                Ok(SketchEntity2D::Spline {
                    id,
                    control_points_mm: pts,
                    closed: flag(&call, "closed")?,
                    segments_per_span,
                    construction: flag(&call, "construction")?,
                })
            } else {
                allow_named(&call, &["blob", "closed", "construction"])?;
                Ok(SketchEntity2D::SplineBlob {
                    id,
                    blob: BlobHandle::from(as_blob(must_named(&call, "blob")?)?),
                    closed: flag(&call, "closed")?,
                    construction: flag(&call, "construction")?,
                })
            }
        }
        "ellipse" => {
            allow_named(
                &call,
                &["center", "rx", "ry", "segments", "construction"],
            )?;
            let segments = match named(&call, "segments") {
                None => None,
                Some(fx) => Some(as_u32(fx)?),
            };
            Ok(SketchEntity2D::Ellipse {
                id,
                center_mm: as_vec2(must_named(&call, "center")?)?,
                rx_mm: as_f64(must_named(&call, "rx")?)?,
                ry_mm: as_f64(must_named(&call, "ry")?)?,
                segments,
                construction: flag(&call, "construction")?,
            })
        }
        _ => Err(fail(
            fx,
            "expected `point`, `line`, `arc`, `circle`, `rectangle`, `ellipse`, or `spline`",
        )),
    }
}

fn as_profile(fx: &Fx) -> Result<SketchProfileLoop, BridgeError> {
    let call = as_call(fx)?;
    if call.name != "loop" {
        return Err(fail(fx, "expected `loop(...)`"));
    }
    allow_named(&call, &["entities", "outer"])?;
    let id = as_text(one_positional(&call)?)?;
    let entity_ids = as_list(must_named(&call, "entities")?)?
        .iter()
        .map(as_text)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SketchProfileLoop {
        id,
        entity_ids,
        is_outer: flag(&call, "outer")?,
    })
}

fn as_constraint(fx: &Fx) -> Result<SketchConstraint2D, BridgeError> {
    let call = as_call(fx)?;
    let name = opt_name(&call)?;
    match call.name {
        "coincident" => {
            allow_named(&call, &["name", "a", "b"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Coincident {
                name,
                a: as_point_ref(must_named(&call, "a")?)?,
                b: as_point_ref(must_named(&call, "b")?)?,
            })
        }
        "point_on_line" => {
            allow_named(&call, &["name", "point", "line"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::PointOnLine {
                name,
                point: as_point_ref(must_named(&call, "point")?)?,
                line: as_text(must_named(&call, "line")?)?,
            })
        }
        "horizontal" => {
            allow_named(&call, &["name", "entity"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Horizontal {
                name,
                entity: as_text(must_named(&call, "entity")?)?,
            })
        }
        "vertical" => {
            allow_named(&call, &["name", "entity"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Vertical {
                name,
                entity: as_text(must_named(&call, "entity")?)?,
            })
        }
        "parallel" | "perpendicular" | "tangent" | "concentric" | "equal" => {
            allow_named(&call, &["name", "a", "b"])?;
            expect_positional(&call, 0)?;
            let a = as_text(must_named(&call, "a")?)?;
            let b = as_text(must_named(&call, "b")?)?;
            Ok(match call.name {
                "parallel" => SketchConstraint2D::Parallel { name, a, b },
                "perpendicular" => SketchConstraint2D::Perpendicular { name, a, b },
                "tangent" => SketchConstraint2D::Tangent { name, a, b },
                "concentric" => SketchConstraint2D::Concentric { name, a, b },
                _ => SketchConstraint2D::Equal { name, a, b },
            })
        }
        "symmetric" => {
            allow_named(&call, &["name", "a", "b", "axis"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Symmetric {
                name,
                a: as_point_ref(must_named(&call, "a")?)?,
                b: as_point_ref(must_named(&call, "b")?)?,
                axis_line: as_text(must_named(&call, "axis")?)?,
            })
        }
        "distance" => {
            allow_named(&call, &["name", "a", "b", "value"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Distance {
                name,
                a: as_point_ref(must_named(&call, "a")?)?,
                b: as_point_ref(must_named(&call, "b")?)?,
                value: as_sketch_dim(must_named(&call, "value")?)?,
            })
        }
        "radius" => {
            allow_named(&call, &["name", "entity", "value"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Radius {
                name,
                entity: as_text(must_named(&call, "entity")?)?,
                value: as_sketch_dim(must_named(&call, "value")?)?,
            })
        }
        "diameter" => {
            allow_named(&call, &["name", "entity", "value"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Diameter {
                name,
                entity: as_text(must_named(&call, "entity")?)?,
                value: as_sketch_dim(must_named(&call, "value")?)?,
            })
        }
        "angle" => {
            allow_named(&call, &["name", "a", "b", "value"])?;
            expect_positional(&call, 0)?;
            Ok(SketchConstraint2D::Angle {
                name,
                a: as_text(must_named(&call, "a")?)?,
                b: as_text(must_named(&call, "b")?)?,
                value: as_sketch_dim(must_named(&call, "value")?)?,
            })
        }
        _ => Err(fail(
            fx,
            format!("unknown sketch constraint `{}`", call.name),
        )),
    }
}

fn as_point_ref(fx: &Fx) -> Result<SketchPointRef, BridgeError> {
    let call = as_call(fx)?;
    match call.name {
        "point" => {
            allow_named(&call, &[])?;
            Ok(SketchPointRef::point(as_text(one_positional(&call)?)?))
        }
        "anchor" => {
            allow_named(&call, &[])?;
            expect_positional(&call, 2)?;
            let entity_id = as_text(&call.positional[0])?;
            let anchor_name = as_text(&call.positional[1])?;
            let anchor = anchor_from(&anchor_name).ok_or_else(|| {
                fail(
                    &call.positional[1],
                    "anchor must be `start`, `end`, `center`, `midpoint`, or `point`",
                )
            })?;
            Ok(SketchPointRef::anchor(entity_id, anchor))
        }
        _ => Err(fail(fx, "expected `point(...)` or `anchor(...)`")),
    }
}

fn as_sketch_dim(fx: &Fx) -> Result<SketchDimValue, BridgeError> {
    match &fx.kind {
        FxKind::Quantity(quantity) => Ok(SketchDimValue::Literal(*quantity)),
        FxKind::Param(id) => Ok(SketchDimValue::param(id.clone())),
        _ => Err(fail(fx, "expected a quantity or `param(...)`")),
    }
}

fn as_string_map(fx: &Fx) -> Result<BTreeMap<String, String>, BridgeError> {
    let FxKind::Map(entries) = &fx.kind else {
        return Err(fail(fx, "expected `{ key = \"value\" }`"));
    };
    let mut map = BTreeMap::new();
    for (key, value) in entries {
        if map.insert(key.clone(), as_string(value)?).is_some() {
            return Err(fail(value, format!("duplicate map key `{key}`")));
        }
    }
    Ok(map)
}

impl Cur<'_> {
    fn as_dim(&mut self, fx: &Fx) -> Result<DimInput, BridgeError> {
        match &fx.kind {
            FxKind::Quantity(quantity) => Ok(DimInput::quantity(*quantity)),
            FxKind::Param(id) => Ok(DimInput::param(id.clone())),
            FxKind::Node { id, port } => Ok(DimInput::NodeOutput {
                node_id: NodeId::from(id.clone()),
                port: port.clone(),
            }),
            FxKind::Ident(_) | FxKind::Str(_) => Ok(DimInput::node(self.as_node_id(fx)?)),
            _ => Err(fail(
                fx,
                "expected a quantity, `param(...)`, or a node reference",
            )),
        }
    }

    fn as_int(&mut self, fx: &Fx) -> Result<IntInput, BridgeError> {
        match &fx.kind {
            FxKind::Int(value) => Ok(IntInput::literal(*value)),
            FxKind::Param(id) => Ok(IntInput::param(id.clone())),
            FxKind::Node { id, port } => Ok(IntInput::NodeOutput {
                node_id: NodeId::from(id.clone()),
                port: port.clone(),
            }),
            FxKind::Ident(_) | FxKind::Str(_) => Ok(IntInput::node(self.as_node_id(fx)?)),
            _ => Err(fail(
                fx,
                "expected an integer, `param(...)`, or a node reference",
            )),
        }
    }

    fn as_node_id(&mut self, fx: &Fx) -> Result<NodeId, BridgeError> {
        match &fx.kind {
            FxKind::Ident(id) | FxKind::Str(id) => {
                if id.trim().is_empty() {
                    return Err(fail(fx, "node id is empty"));
                }
                self.node_refs.push((id.clone(), fx.at.0, fx.at.1));
                Ok(NodeId::from(id.clone()))
            }
            FxKind::Node { id, port } => {
                if port != OUTPUT_PORT {
                    return Err(fail(fx, "this node reference does not take a port"));
                }
                Ok(NodeId::from(id.clone()))
            }
            _ => Err(fail(fx, "expected a node reference")),
        }
    }

    fn as_node_list(&mut self, fx: &Fx) -> Result<Vec<NodeId>, BridgeError> {
        as_list(fx)?
            .iter()
            .map(|item| self.as_node_id(item))
            .collect()
    }

    fn parse_block(&mut self) -> Result<BTreeMap<String, Fx>, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LBrace), "`{`")?;
        let mut fields = BTreeMap::new();
        if self.eat(|kind| matches!(kind, TokenKind::RBrace)) {
            return Ok(fields);
        }
        loop {
            let at = self.location();
            let key = self.take_key()?;
            self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
            let value = self.parse_fx()?;
            if fields.insert(key.clone(), value).is_some() {
                return Err(BridgeError::at(
                    at.0,
                    at.1,
                    format!("duplicate field `{key}`"),
                ));
            }
            if !self.continue_list(|kind| matches!(kind, TokenKind::RBrace))? {
                break;
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RBrace), "`}`")?;
        Ok(fields)
    }

    fn parse_fx(&mut self) -> Result<Fx, BridgeError> {
        let at = self.location();
        if self.eat(|kind| matches!(kind, TokenKind::Minus)) {
            let number = self.take_number()?;
            return self.number_fx(number, true, at);
        }
        if self.eat(|kind| matches!(kind, TokenKind::True)) {
            return Ok(Fx {
                at,
                kind: FxKind::Bool(true),
            });
        }
        if self.eat(|kind| matches!(kind, TokenKind::False)) {
            return Ok(Fx {
                at,
                kind: FxKind::Bool(false),
            });
        }
        if self.at(|kind| matches!(kind, TokenKind::Str(_))) {
            return Ok(Fx {
                at,
                kind: FxKind::Str(self.take_string()?),
            });
        }
        if self.at(|kind| matches!(kind, TokenKind::Number { .. })) {
            let number = self.take_number()?;
            return self.number_fx(number, false, at);
        }
        if self.at(|kind| matches!(kind, TokenKind::LBracket)) {
            return self.parse_list(at);
        }
        if self.at(|kind| matches!(kind, TokenKind::LBrace)) {
            return self.parse_map(at);
        }
        if self.at(|kind| matches!(kind, TokenKind::At)) {
            return self.parse_handle(at);
        }
        if self.can_take_name() {
            let name = self.take_name()?;
            if matches!(name.as_str(), "vec2" | "vec3" | "param" | "node" | "out")
                && self.at(|kind| matches!(kind, TokenKind::LParen))
            {
                return self.parse_special(at, &name);
            }
            if self.at(|kind| matches!(kind, TokenKind::LParen)) {
                return self.parse_call(at, name);
            }
            return Ok(Fx {
                at,
                kind: FxKind::Ident(name),
            });
        }
        Err(self.unexpected("a value"))
    }

    fn parse_special(&mut self, at: (usize, usize), name: &str) -> Result<Fx, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let kind = match name {
            "vec2" => {
                let x = self.parse_component()?;
                self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
                let y = self.parse_component()?;
                let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
                FxKind::Vec2([x, y])
            }
            "vec3" => {
                let x = self.parse_component()?;
                self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
                let y = self.parse_component()?;
                self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
                let z = self.parse_component()?;
                let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
                FxKind::Vec3([x, y, z])
            }
            "param" => {
                let id = self.take_binding()?;
                if id.trim().is_empty() {
                    return Err(self.error("parameter name is empty"));
                }
                let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
                self.param_uses.push((id.clone(), at.0, at.1));
                FxKind::Param(id)
            }
            "node" => {
                let id = self.take_binding()?;
                if id.trim().is_empty() {
                    return Err(self.error("node id is empty"));
                }
                let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
                self.node_refs.push((id.clone(), at.0, at.1));
                FxKind::Node {
                    id,
                    port: OUTPUT_PORT.to_owned(),
                }
            }
            "out" => {
                let id = self.take_binding()?;
                if id.trim().is_empty() {
                    return Err(self.error("node id is empty"));
                }
                let port = if self.eat(|kind| matches!(kind, TokenKind::Comma))
                    && !self.at(|kind| matches!(kind, TokenKind::RParen))
                {
                    let port = self.take_binding()?;
                    let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
                    port
                } else {
                    OUTPUT_PORT.to_owned()
                };
                if port.trim().is_empty() {
                    return Err(self.error("port name is empty"));
                }
                self.node_refs.push((id.clone(), at.0, at.1));
                FxKind::Node { id, port }
            }
            _ => unreachable!("special call name"),
        };
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(Fx { at, kind })
    }

    fn parse_call(&mut self, at: (usize, usize), name: String) -> Result<Fx, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let mut positional = Vec::new();
        let mut named = Vec::new();
        if !self.eat(|kind| matches!(kind, TokenKind::RParen)) {
            loop {
                if self.looks_like_named() {
                    let key_at = self.location();
                    let key = self.take_key()?;
                    self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                    if named.iter().any(|(existing, _)| existing == &key) {
                        return Err(BridgeError::at(
                            key_at.0,
                            key_at.1,
                            format!("duplicate field `{key}`"),
                        ));
                    }
                    named.push((key, self.parse_fx()?));
                } else {
                    if !named.is_empty() {
                        return Err(self.error("positional argument after a named argument"));
                    }
                    positional.push(self.parse_fx()?);
                }
                if !self.continue_list(|kind| matches!(kind, TokenKind::RParen))? {
                    break;
                }
            }
            self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        }
        Ok(Fx {
            at,
            kind: FxKind::Call {
                name,
                positional,
                named,
            },
        })
    }

    fn parse_list(&mut self, at: (usize, usize)) -> Result<Fx, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LBracket), "`[`")?;
        let mut items = Vec::new();
        if !self.at(|kind| matches!(kind, TokenKind::RBracket)) {
            loop {
                items.push(self.parse_fx()?);
                if !self.continue_list(|kind| matches!(kind, TokenKind::RBracket))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RBracket), "`]`")?;
        Ok(Fx {
            at,
            kind: FxKind::List(items),
        })
    }

    fn parse_map(&mut self, at: (usize, usize)) -> Result<Fx, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LBrace), "`{`")?;
        let mut entries = Vec::new();
        if !self.at(|kind| matches!(kind, TokenKind::RBrace)) {
            loop {
                let key_at = self.location();
                let key = self.take_key()?;
                self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                if entries.iter().any(|(existing, _)| existing == &key) {
                    return Err(BridgeError::at(
                        key_at.0,
                        key_at.1,
                        format!("duplicate map key `{key}`"),
                    ));
                }
                entries.push((key, self.parse_fx()?));
                if !self.continue_list(|kind| matches!(kind, TokenKind::RBrace))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RBrace), "`}`")?;
        Ok(Fx {
            at,
            kind: FxKind::Map(entries),
        })
    }

    fn parse_handle(&mut self, at: (usize, usize)) -> Result<Fx, BridgeError> {
        self.bump();
        let keyword = self.take_ident()?;
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        if keyword == "extref" {
            let ref_id = self.take_binding()?;
            let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
            self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
            if ref_id.trim().is_empty() {
                return Err(BridgeError::at(at.0, at.1, "external_ref id is empty"));
            }
            self.extref_uses.push((ref_id.clone(), at.0, at.1));
            return Ok(Fx {
                at,
                kind: FxKind::Ext(ref_id),
            });
        }
        let body = self.take_string()?;
        let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        if keyword == "topo" {
            return Ok(Fx {
                at,
                kind: FxKind::Topo(body),
            });
        }
        let handle = if keyword == "blob_ref" {
            body
        } else if let Some(handle) = blob_handle_from_keyword(&keyword, &body) {
            handle
        } else {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("unknown handle `@{keyword}`"),
            ));
        };
        if handle.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "blob handle is empty"));
        }
        self.blob_uses.push((handle.clone(), at.0, at.1));
        Ok(Fx {
            at,
            kind: FxKind::Blob(handle),
        })
    }

    fn parse_component(&mut self) -> Result<f64, BridgeError> {
        let at = self.location();
        let negated = self.eat(|kind| matches!(kind, TokenKind::Minus));
        let number = self.take_number()?;
        if number.unit.is_some() {
            return Err(BridgeError::at(
                at.0,
                at.1,
                "vector component must be a plain number",
            ));
        }
        let signed = if negated { -number.value } else { number.value };
        if !signed.is_finite() {
            return Err(BridgeError::at(at.0, at.1, "number is not finite"));
        }
        Ok(if signed == 0.0 { 0.0 } else { signed })
    }

    fn number_fx(
        &self,
        number: NumberTok,
        negated: bool,
        at: (usize, usize),
    ) -> Result<Fx, BridgeError> {
        let signed = if negated { -number.value } else { number.value };
        if let Some(suffix) = number.unit {
            return match quantity_from_suffix(signed, &suffix) {
                SuffixParse::Quantity(quantity) => Ok(Fx {
                    at,
                    kind: FxKind::Quantity(quantity),
                }),
                SuffixParse::Unknown => Err(BridgeError::at(
                    at.0,
                    at.1,
                    format!("unknown unit `{suffix}`"),
                )),
                SuffixParse::NonFinite => Err(BridgeError::at(
                    at.0,
                    at.1,
                    "quantity is not a finite number",
                )),
            };
        }
        if is_float_literal(&number.raw) {
            if !signed.is_finite() {
                return Err(BridgeError::at(at.0, at.1, "number is not finite"));
            }
            let signed = if signed == 0.0 { 0.0 } else { signed };
            return Ok(Fx {
                at,
                kind: FxKind::Float(signed),
            });
        }
        let integer = parse_i64_literal(&number.raw, negated)
            .map_err(|message| BridgeError::at(at.0, at.1, message))?;
        Ok(Fx {
            at,
            kind: FxKind::Int(integer),
        })
    }

    fn can_take_name(&self) -> bool {
        matches!(
            self.tokens[self.cursor].kind,
            TokenKind::Ident(_)
                | TokenKind::Part
                | TokenKind::Assembly
                | TokenKind::Let
                | TokenKind::Return
                | TokenKind::Sketch
                | TokenKind::Const
        )
    }

    fn take_name(&mut self) -> Result<String, BridgeError> {
        let name = match &self.tokens[self.cursor].kind {
            TokenKind::Ident(name) => name.clone(),
            TokenKind::Part => "part".to_owned(),
            TokenKind::Assembly => "assembly".to_owned(),
            TokenKind::Let => "let".to_owned(),
            TokenKind::Return => "return".to_owned(),
            TokenKind::Sketch => "sketch".to_owned(),
            TokenKind::Const => "const".to_owned(),
            _ => return Err(self.unexpected("an identifier")),
        };
        self.bump();
        Ok(name)
    }

    fn take_binding(&mut self) -> Result<String, BridgeError> {
        if self.at(|kind| matches!(kind, TokenKind::Str(_))) {
            self.take_string()
        } else {
            self.take_name()
        }
    }

    fn take_key(&mut self) -> Result<String, BridgeError> {
        self.take_binding()
    }

    fn take_ident(&mut self) -> Result<String, BridgeError> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.bump();
                Ok(name)
            }
            _ => Err(self.unexpected("an identifier")),
        }
    }

    fn take_string(&mut self) -> Result<String, BridgeError> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Str(value) => {
                let value = value.clone();
                self.bump();
                Ok(value)
            }
            _ => Err(self.unexpected("a string")),
        }
    }

    fn take_number(&mut self) -> Result<NumberTok, BridgeError> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Number {
                value,
                raw_number,
                unit,
            } => {
                let number = NumberTok {
                    value: *value,
                    raw: raw_number.clone(),
                    unit: unit.clone(),
                };
                self.bump();
                Ok(number)
            }
            _ => Err(self.unexpected("a number")),
        }
    }

    fn looks_like_named(&self) -> bool {
        let key =
            self.can_take_name() || matches!(self.tokens[self.cursor].kind, TokenKind::Str(_));
        key && self.next_is(|kind| matches!(kind, TokenKind::Eq))
    }

    fn continue_list(&mut self, close: impl Fn(&TokenKind) -> bool) -> Result<bool, BridgeError> {
        if self.eat(|kind| matches!(kind, TokenKind::Comma)) {
            Ok(!self.at(&close))
        } else {
            Ok(false)
        }
    }

    fn at(&self, pred: impl Fn(&TokenKind) -> bool) -> bool {
        pred(&self.tokens[self.cursor].kind)
    }

    fn next_is(&self, pred: impl Fn(&TokenKind) -> bool) -> bool {
        self.tokens
            .get(self.cursor + 1)
            .is_some_and(|token| pred(&token.kind))
    }

    fn eat(&mut self, pred: impl Fn(&TokenKind) -> bool) -> bool {
        if self.at(pred) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, pred: impl Fn(&TokenKind) -> bool, what: &str) -> Result<(), BridgeError> {
        if self.eat(pred) {
            Ok(())
        } else {
            Err(self.unexpected(what))
        }
    }

    fn bump(&mut self) {
        if self.cursor + 1 < self.tokens.len() {
            self.cursor += 1;
        }
    }

    fn location(&self) -> (usize, usize) {
        let span = self.tokens[self.cursor].span;
        (span.start_line, span.start_col)
    }

    fn error(&self, message: impl Into<String>) -> BridgeError {
        let (line, column) = self.location();
        BridgeError::at(line, column, message)
    }

    fn unexpected(&self, what: &str) -> BridgeError {
        let lexeme = self.tokens[self.cursor].lexeme.as_str();
        let found = if lexeme.is_empty() {
            "end of file".to_owned()
        } else {
            format!("`{lexeme}`")
        };
        self.error(format!("expected {what}, found {found}"))
    }
}

struct NumberTok {
    value: f64,
    raw: String,
    unit: Option<String>,
}

fn as_call(fx: &Fx) -> Result<Call<'_>, BridgeError> {
    match &fx.kind {
        FxKind::Call {
            name,
            positional,
            named,
        } => Ok(Call {
            at: fx.at,
            name,
            positional,
            named,
        }),
        _ => Err(fail(fx, "expected a call")),
    }
}

fn named<'a>(call: &Call<'a>, key: &str) -> Option<&'a Fx> {
    call.named
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn must_named<'a>(call: &Call<'a>, key: &str) -> Result<&'a Fx, BridgeError> {
    named(call, key)
        .ok_or_else(|| BridgeError::at(call.at.0, call.at.1, format!("missing `{key}`")))
}

fn allow_named(call: &Call<'_>, allowed: &[&str]) -> Result<(), BridgeError> {
    for (key, fx) in call.named {
        if !allowed.contains(&key.as_str()) {
            return Err(fail(fx, format!("unknown field `{key}`")));
        }
    }
    Ok(())
}

fn one_positional<'a>(call: &Call<'a>) -> Result<&'a Fx, BridgeError> {
    expect_positional(call, 1)?;
    Ok(&call.positional[0])
}

fn expect_positional(call: &Call<'_>, count: usize) -> Result<(), BridgeError> {
    if call.positional.len() == count {
        Ok(())
    } else {
        Err(BridgeError::at(
            call.at.0,
            call.at.1,
            format!(
                "`{}` expected {count} positional arguments, found {}",
                call.name,
                call.positional.len()
            ),
        ))
    }
}

fn flag(call: &Call<'_>, key: &str) -> Result<bool, BridgeError> {
    match named(call, key) {
        None => Ok(false),
        Some(fx) => as_bool(fx),
    }
}

fn opt_name(call: &Call<'_>) -> Result<Option<String>, BridgeError> {
    match named(call, "name") {
        None => Ok(None),
        Some(fx) => Ok(Some(as_text(fx)?)),
    }
}

fn allow_keys(fields: &BTreeMap<String, Fx>, allowed: &[&str]) -> Result<(), BridgeError> {
    for (key, fx) in fields {
        if !allowed.contains(&key.as_str()) {
            return Err(fail(fx, format!("unknown field `{key}`")));
        }
    }
    Ok(())
}

fn required<'a>(
    fields: &'a BTreeMap<String, Fx>,
    key: &str,
    at: (usize, usize),
) -> Result<&'a Fx, BridgeError> {
    fields
        .get(key)
        .ok_or_else(|| BridgeError::at(at.0, at.1, format!("missing `{key}`")))
}

fn as_list(fx: &Fx) -> Result<&[Fx], BridgeError> {
    match &fx.kind {
        FxKind::List(items) => Ok(items),
        _ => Err(fail(fx, "expected a list")),
    }
}

fn as_bool(fx: &Fx) -> Result<bool, BridgeError> {
    match &fx.kind {
        FxKind::Bool(value) => Ok(*value),
        _ => Err(fail(fx, "expected `true` or `false`")),
    }
}

fn as_string(fx: &Fx) -> Result<String, BridgeError> {
    match &fx.kind {
        FxKind::Str(value) => Ok(value.clone()),
        _ => Err(fail(fx, "expected a string")),
    }
}

fn as_text(fx: &Fx) -> Result<String, BridgeError> {
    match &fx.kind {
        FxKind::Ident(value) | FxKind::Str(value) => Ok(value.clone()),
        _ => Err(fail(fx, "expected a name or string")),
    }
}

fn as_topo(fx: &Fx) -> Result<String, BridgeError> {
    match &fx.kind {
        FxKind::Topo(value) | FxKind::Str(value) => Ok(value.clone()),
        _ => Err(fail(fx, "expected `@topo(\"...\")`")),
    }
}

fn as_blob(fx: &Fx) -> Result<String, BridgeError> {
    match &fx.kind {
        FxKind::Blob(value) => Ok(value.clone()),
        _ => Err(fail(fx, "expected a blob handle")),
    }
}

fn as_ext(fx: &Fx) -> Result<String, BridgeError> {
    match &fx.kind {
        FxKind::Ext(value) => Ok(value.clone()),
        _ => Err(fail(fx, "expected `@extref(\"...\")`")),
    }
}

fn as_vec3(fx: &Fx) -> Result<[f64; 3], BridgeError> {
    match &fx.kind {
        FxKind::Vec3(value) => Ok(*value),
        _ => Err(fail(fx, "expected `vec3(...)`")),
    }
}

fn as_vec2(fx: &Fx) -> Result<[f64; 2], BridgeError> {
    match &fx.kind {
        FxKind::Vec2(value) => Ok(*value),
        _ => Err(fail(fx, "expected `vec2(...)`")),
    }
}

fn as_f64(fx: &Fx) -> Result<f64, BridgeError> {
    match &fx.kind {
        FxKind::Float(value) => Ok(if *value == 0.0 { 0.0 } else { *value }),
        FxKind::Int(value) => {
            let number = *value as f64;
            if number as i64 != *value {
                return Err(fail(fx, "integer is not an exact float"));
            }
            Ok(if number == 0.0 { 0.0 } else { number })
        }
        _ => Err(fail(fx, "expected a number")),
    }
}

fn as_u32(fx: &Fx) -> Result<u32, BridgeError> {
    match &fx.kind {
        FxKind::Int(value) if *value >= 0 && *value <= i64::from(u32::MAX) => Ok(*value as u32),
        _ => Err(fail(fx, "expected a non-negative integer")),
    }
}

fn fail(fx: &Fx, message: impl Into<String>) -> BridgeError {
    BridgeError::at(fx.at.0, fx.at.1, message)
}

fn datum_from(name: &str) -> Option<DatumPlane> {
    Some(match name {
        "xy" => DatumPlane::Xy,
        "xz" => DatumPlane::Xz,
        "yz" => DatumPlane::Yz,
        _ => return None,
    })
}

fn anchor_from(name: &str) -> Option<SketchAnchor> {
    Some(match name {
        "point" => SketchAnchor::Point,
        "start" => SketchAnchor::Start,
        "end" => SketchAnchor::End,
        "center" => SketchAnchor::Center,
        "midpoint" => SketchAnchor::Midpoint,
        _ => return None,
    })
}

fn body_op_from(name: &str) -> Option<FeatureBodyOp> {
    Some(match name {
        "new_body" => FeatureBodyOp::NewBody,
        "join" => FeatureBodyOp::Join,
        "cut" => FeatureBodyOp::Cut,
        "intersect" => FeatureBodyOp::Intersect,
        _ => return None,
    })
}

fn fastener_from(name: &str) -> Option<FastenerHoleStandard> {
    Some(match name {
        "iso4762_counterbore" => FastenerHoleStandard::Iso4762Counterbore,
        "iso10642_countersink" => FastenerHoleStandard::Iso10642Countersink,
        "metric_tap_drill" => FastenerHoleStandard::MetricTapDrill,
        "metric_clearance" => FastenerHoleStandard::MetricClearance,
        _ => return None,
    })
}

fn is_float_literal(raw: &str) -> bool {
    raw.contains('.') || raw.contains('e') || raw.contains('E')
}

fn parse_i64_literal(raw: &str, negated: bool) -> Result<i64, String> {
    let digits = raw.replace('_', "");
    let text = if negated {
        format!("-{digits}")
    } else {
        digits
    };
    text.parse::<i64>()
        .map_err(|_| format!("integer `{text}` is out of range"))
}

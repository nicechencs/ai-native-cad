//! `FeatureSpec` -> deterministic `.kcl` block.

use cad_ir::{
    DesignAssertionKind, DimInput, DirectFaceEditOp, FeatureBodyOp, FeatureSpec, HoleDepthSpec,
    HoleProfileSpec, IntInput, IrNode, LinearPatternAxisSpec, MateKindSpec, SketchAnchor,
    SketchConstraint2D, SketchDimValue, SketchEntity2D, SketchPlaneSpec, SketchPointRef,
    SweepPathSpec, OUTPUT_PORT,
};

use crate::model::{
    format_binding, format_blob_ref, format_f64, format_key, format_node_ref, format_quantity,
    format_scalar, format_string, operator_name, type_name,
};

const ZERO: [f64; 3] = [0.0, 0.0, 0.0];
const Z_AXIS: [f64; 3] = [0.0, 0.0, 1.0];
const NEG_Z: [f64; 3] = [0.0, 0.0, -1.0];

/// Emit `node` as a feature block when it is exactly a `from_feature_spec` node.
///
/// Annotations (`@name`, `@suppressed`, …) stay in the caller. Returns `None`
/// for legacy port maps so existing `.kcl` text is unchanged.
pub(crate) fn try_emit_feature(node: &IrNode) -> Option<String> {
    let spec = node.try_feature_spec().ok()?;
    let rebuilt =
        IrNode::from_feature_spec(node.id.0.as_str(), node.name.clone(), spec.clone()).ok()?;
    if rebuilt.kind != node.kind
        || rebuilt.output_type != node.output_type
        || rebuilt.inputs != node.inputs
    {
        return None;
    }
    Some(render_let(&node.id.0, &spec))
}

fn render_let(id: &str, spec: &FeatureSpec) -> String {
    let body = render_fields(&fields_of(spec));
    let head = format!("let {} = {}", format_binding(id), feature_operator(spec));
    let tail = format!(" -> {}", type_name(spec.output_type()));
    if body.is_empty() {
        format!("{head} {{}}{tail}")
    } else {
        format!("{head} {{\n{body}}}{tail}")
    }
}

fn feature_operator(spec: &FeatureSpec) -> &'static str {
    match spec {
        FeatureSpec::StdFastenerHole { .. } => "std_fastener_hole",
        other => operator_name(&other.node_kind()),
    }
}

struct Field {
    key: &'static str,
    value: String,
}

fn render_fields(fields: &[Field]) -> String {
    let mut out = String::new();
    for field in fields {
        out.push_str("    ");
        out.push_str(field.key);
        out.push_str(" = ");
        out.push_str(&field.value);
        out.push_str(",\n");
    }
    out
}

fn field(key: &'static str, value: impl Into<String>) -> Field {
    Field {
        key,
        value: value.into(),
    }
}

fn fields_of(spec: &FeatureSpec) -> Vec<Field> {
    match spec {
        FeatureSpec::PrimitiveBox {
            width,
            depth,
            height,
            origin_mm,
        } => {
            let mut fields = vec![
                field("width", emit_dim(width)),
                field("depth", emit_dim(depth)),
                field("height", emit_dim(height)),
            ];
            push_vec3(&mut fields, "origin", origin_mm, ZERO);
            fields
        }
        FeatureSpec::PrimitiveCylinder {
            radius,
            height,
            origin_mm,
            axis,
        } => {
            let mut fields = vec![
                field("radius", emit_dim(radius)),
                field("height", emit_dim(height)),
            ];
            push_vec3(&mut fields, "origin", origin_mm, ZERO);
            push_vec3(&mut fields, "axis", axis, Z_AXIS);
            fields
        }
        FeatureSpec::Sketch2D(sketch) => {
            let mut fields = vec![field("plane", emit_plane(&sketch.plane))];
            if !sketch.entities.is_empty() {
                let items = sketch.entities.iter().map(emit_entity).collect::<Vec<_>>();
                fields.push(field("entities", render_lines(&items)));
            }
            if !sketch.profiles.is_empty() {
                let items = sketch.profiles.iter().map(emit_profile).collect::<Vec<_>>();
                fields.push(field("profiles", render_lines(&items)));
            }
            if !sketch.constraints.is_empty() {
                let items = sketch
                    .constraints
                    .iter()
                    .map(emit_constraint)
                    .collect::<Vec<_>>();
                fields.push(field("constraints", render_lines(&items)));
            }
            if let Some(dof) = sketch.remaining_dof {
                fields.push(field("dof", dof.to_string()));
            }
            fields
        }
        FeatureSpec::Extrude {
            sketch,
            distance,
            direction,
            symmetric,
            operation,
            target,
        } => {
            let mut fields = vec![
                field("sketch", emit_node(sketch)),
                field("distance", emit_dim(distance)),
            ];
            if let Some(direction) = direction {
                fields.push(field("direction", emit_vec3(direction)));
            }
            push_bool(&mut fields, "symmetric", *symmetric, false);
            if *operation != FeatureBodyOp::NewBody {
                fields.push(field("operation", body_op_name(*operation)));
            }
            if let Some(target) = target {
                fields.push(field("target", emit_node(target)));
            }
            fields
        }
        FeatureSpec::Revolve {
            sketch,
            axis_origin_mm,
            axis_direction,
            angle,
            operation,
            target,
        } => {
            let mut fields = vec![field("sketch", emit_node(sketch))];
            push_vec3(&mut fields, "origin", axis_origin_mm, ZERO);
            push_vec3(&mut fields, "axis", axis_direction, Z_AXIS);
            fields.push(field("angle", emit_dim(angle)));
            if *operation != FeatureBodyOp::NewBody {
                fields.push(field("operation", body_op_name(*operation)));
            }
            if let Some(target) = target {
                fields.push(field("target", emit_node(target)));
            }
            fields
        }
        FeatureSpec::Sweep {
            profile,
            path,
            keep_normal,
        } => {
            let mut fields = vec![
                field("profile", emit_node(profile)),
                field("path", emit_sweep_path(path)),
            ];
            push_bool(&mut fields, "keep_normal", *keep_normal, false);
            fields
        }
        FeatureSpec::Loft {
            sections,
            guides,
            closed,
            as_surface,
        } => {
            let mut fields = vec![field("sections", emit_node_list(sections))];
            if !guides.is_empty() {
                fields.push(field("guides", emit_node_list(guides)));
            }
            push_bool(&mut fields, "closed", *closed, false);
            push_bool(&mut fields, "as_surface", *as_surface, false);
            fields
        }
        FeatureSpec::Hole {
            solid,
            target_face,
            origin_mm,
            axis,
            diameter,
            depth,
            profile,
        } => {
            let mut fields = vec![field("solid", emit_node(solid))];
            if let Some(face) = target_face {
                fields.push(field("face", emit_topo(face)));
            }
            push_vec3(&mut fields, "origin", origin_mm, ZERO);
            push_vec3(&mut fields, "axis", axis, NEG_Z);
            fields.push(field("diameter", emit_dim(diameter)));
            if let Some(depth) = emit_depth(depth) {
                fields.push(field("depth", depth));
            }
            if let Some(profile) = emit_hole_profile(profile) {
                fields.push(field("profile", profile));
            }
            fields
        }
        FeatureSpec::StdFastenerHole {
            solid,
            target_face,
            origin_mm,
            axis,
            standard,
            designation,
            clearance_class,
            depth,
            thread_depth,
        } => {
            let mut fields = vec![field("solid", emit_node(solid))];
            if let Some(face) = target_face {
                fields.push(field("face", emit_topo(face)));
            }
            push_vec3(&mut fields, "origin", origin_mm, ZERO);
            push_vec3(&mut fields, "axis", axis, NEG_Z);
            fields.push(field("standard", fastener_name(*standard)));
            fields.push(field("designation", format_string(designation)));
            if let Some(class) = clearance_class {
                fields.push(field("clearance", format_string(class)));
            }
            if let Some(depth) = emit_depth(depth) {
                fields.push(field("depth", depth));
            }
            if let Some(thread) = thread_depth {
                fields.push(field("thread_depth", emit_dim(thread)));
            }
            fields
        }
        FeatureSpec::Fillet {
            solid,
            edges,
            radius,
        } => vec![
            field("solid", emit_node(solid)),
            field("edges", emit_topo_list(edges)),
            field("radius", emit_dim(radius)),
        ],
        FeatureSpec::Chamfer {
            solid,
            edges,
            distance,
            distance2,
            angle,
        } => {
            let mut fields = vec![
                field("solid", emit_node(solid)),
                field("edges", emit_topo_list(edges)),
                field("distance", emit_dim(distance)),
            ];
            if let Some(distance2) = distance2 {
                fields.push(field("distance2", emit_dim(distance2)));
            }
            if let Some(angle) = angle {
                fields.push(field("angle", emit_dim(angle)));
            }
            fields
        }
        FeatureSpec::Shell {
            solid,
            open_faces,
            thickness,
            inward,
        } => {
            let mut fields = vec![
                field("solid", emit_node(solid)),
                field("open_faces", emit_topo_list(open_faces)),
                field("thickness", emit_dim(thickness)),
            ];
            push_bool(&mut fields, "inward", *inward, true);
            fields
        }
        FeatureSpec::Draft {
            solid,
            faces,
            pull_direction,
            neutral_plane,
            angle,
        } => vec![
            field("solid", emit_node(solid)),
            field("faces", emit_topo_list(faces)),
            field("pull_direction", emit_vec3(pull_direction)),
            field("neutral_plane", emit_plane(neutral_plane)),
            field("angle", emit_dim(angle)),
        ],
        FeatureSpec::LinearPattern {
            solid,
            direction,
            count,
            spacing,
            second_direction,
        } => {
            let mut fields = vec![
                field("solid", emit_node(solid)),
                field("direction", emit_vec3(direction)),
                field("count", emit_int(count)),
                field("spacing", emit_dim(spacing)),
            ];
            if let Some(second) = second_direction {
                fields.push(field("second", emit_pattern_axis(second)));
            }
            fields
        }
        FeatureSpec::PolarPattern {
            solid,
            axis_origin_mm,
            axis_direction,
            count,
            total_angle,
            radius,
            start_angle,
        } => {
            let mut fields = vec![field("solid", emit_node(solid))];
            push_vec3(&mut fields, "axis_origin", axis_origin_mm, ZERO);
            push_vec3(&mut fields, "axis", axis_direction, Z_AXIS);
            fields.push(field("count", emit_int(count)));
            fields.push(field("angle", emit_dim(total_angle)));
            if let Some(radius) = radius {
                fields.push(field("radius", emit_dim(radius)));
            }
            if let Some(start_angle) = start_angle {
                fields.push(field("start_angle", emit_dim(start_angle)));
            }
            fields
        }
        FeatureSpec::Mirror {
            solid,
            plane,
            keep_original,
        } => {
            let mut fields = vec![
                field("solid", emit_node(solid)),
                field("plane", emit_plane(plane)),
            ];
            push_bool(&mut fields, "keep_original", *keep_original, true);
            fields
        }
        FeatureSpec::Boolean {
            op: _,
            target,
            tools,
        } => vec![
            field("target", emit_node(target)),
            field("tools", emit_node_list(tools)),
        ],
        FeatureSpec::DirectFaceEdit { solid, face, edit } => {
            let mut fields = vec![
                field("solid", emit_node(solid)),
                field("face", emit_topo(face)),
            ];
            match edit {
                DirectFaceEditOp::Offset { distance } => {
                    fields.push(field("offset", emit_dim(distance)));
                }
                DirectFaceEditOp::Move { delta_mm } => {
                    fields.push(field("move", emit_vec3(delta_mm)));
                }
                DirectFaceEditOp::DeleteAndHeal => {
                    fields.push(field("delete", "true"));
                }
            }
            fields
        }
        FeatureSpec::ImportStep {
            blob,
            path,
            scale,
            as_reference_only,
        } => {
            let mut fields = Vec::new();
            if let Some(blob) = blob {
                fields.push(field("blob", format_blob_ref(&blob.0)));
            }
            if let Some(path) = path {
                fields.push(field("path", format_string(path)));
            }
            if *scale != 1.0 {
                fields.push(field("scale", format_scalar(*scale)));
            }
            push_bool(&mut fields, "reference_only", *as_reference_only, false);
            fields
        }
        FeatureSpec::ComponentInstance {
            source,
            translation_mm,
            rotation_deg,
            grounded,
        } => {
            let mut fields = Vec::new();
            match source {
                cad_ir::ComponentSourceSpec::LocalNode { node_id } => {
                    fields.push(field("geometry", emit_node(node_id)));
                }
                cad_ir::ComponentSourceSpec::ExternalRef { ref_id } => {
                    fields.push(field(
                        "external_ref",
                        format!("@extref({})", format_string(&ref_id.0)),
                    ));
                }
            }
            push_vec3(&mut fields, "translation", translation_mm, ZERO);
            push_vec3(&mut fields, "rotation", rotation_deg, ZERO);
            push_bool(&mut fields, "grounded", *grounded, false);
            fields
        }
        FeatureSpec::MateConstraint {
            left,
            right,
            left_ref,
            right_ref,
            mate_kind,
            flip,
        } => {
            let mut fields = vec![
                field("left", emit_node(left)),
                field("right", emit_node(right)),
                field("left_ref", emit_topo(left_ref)),
                field("right_ref", emit_topo(right_ref)),
                field("kind", emit_mate_kind(mate_kind)),
            ];
            push_bool(&mut fields, "flip", *flip, false);
            fields
        }
        FeatureSpec::TagTopology {
            solid,
            tag,
            targets,
        } => vec![
            field("solid", emit_node(solid)),
            field("tag", format_string(tag)),
            field("targets", emit_topo_list(targets)),
        ],
        FeatureSpec::DesignAssertion {
            target,
            assertion_id,
            kind,
        } => {
            let mut fields = Vec::new();
            if let Some(target) = target {
                fields.push(field("target", emit_node(target)));
            }
            fields.push(field("id", format_string(assertion_id)));
            fields.push(field("rule", emit_rule(kind)));
            fields
        }
    }
}

fn push_bool(fields: &mut Vec<Field>, key: &'static str, value: bool, default: bool) {
    if value != default {
        fields.push(field(key, if value { "true" } else { "false" }));
    }
}

fn push_vec3(fields: &mut Vec<Field>, key: &'static str, value: &[f64; 3], default: [f64; 3]) {
    if value != &default {
        fields.push(field(key, emit_vec3(value)));
    }
}

fn emit_dim(dim: &DimInput) -> String {
    match dim {
        DimInput::Literal(quantity) => format_quantity(*quantity),
        DimInput::Param { param_id } => format!("param({})", format_binding(&param_id.0)),
        DimInput::NodeOutput { node_id, port } => emit_output(node_id, port),
    }
}

fn emit_int(value: &IntInput) -> String {
    match value {
        IntInput::Literal { value } => value.to_string(),
        IntInput::Param { param_id } => format!("param({})", format_binding(&param_id.0)),
        IntInput::NodeOutput { node_id, port } => emit_output(node_id, port),
    }
}

fn emit_output(node_id: &cad_ir::NodeId, port: &str) -> String {
    if port == OUTPUT_PORT {
        format_node_ref(&node_id.0)
    } else {
        format!(
            "out({}, {})",
            format_binding(&node_id.0),
            format_string(port)
        )
    }
}

fn emit_node(node_id: &cad_ir::NodeId) -> String {
    format_node_ref(&node_id.0)
}

fn emit_vec3(value: &[f64; 3]) -> String {
    format!(
        "vec3({}, {}, {})",
        format_f64(value[0]),
        format_f64(value[1]),
        format_f64(value[2])
    )
}

fn emit_vec2(value: &[f64; 2]) -> String {
    format!("vec2({}, {})", format_f64(value[0]), format_f64(value[1]))
}

fn emit_topo(expr: &str) -> String {
    format!("@topo({})", format_string(expr))
}

fn emit_id(id: &str) -> String {
    format_binding(id)
}

fn emit_topo_list(items: &[String]) -> String {
    render_inline(&items.iter().map(|item| emit_topo(item)).collect::<Vec<_>>())
}

fn emit_node_list(items: &[cad_ir::NodeId]) -> String {
    render_inline(&items.iter().map(emit_node).collect::<Vec<_>>())
}

fn render_inline(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

fn render_lines(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_owned();
    }
    let mut out = String::from("[\n");
    for item in items {
        out.push_str("        ");
        out.push_str(item);
        out.push_str(",\n");
    }
    out.push_str("    ]");
    out
}

fn emit_plane(plane: &SketchPlaneSpec) -> String {
    match plane {
        SketchPlaneSpec::Datum { plane, offset } => match offset {
            Some(offset) => format!(
                "datum({}, offset = {})",
                datum_name(*plane),
                emit_sketch_dim(offset)
            ),
            None => format!("datum({})", datum_name(*plane)),
        },
        SketchPlaneSpec::FaceRef { face, offset } => match offset {
            Some(offset) => format!(
                "face({}, offset = {})",
                emit_topo(face),
                emit_sketch_dim(offset)
            ),
            None => format!("face({})", emit_topo(face)),
        },
        SketchPlaneSpec::Custom {
            origin_mm,
            normal,
            x_axis,
        } => {
            let mut text = format!(
                "custom(origin = {}, normal = {}",
                emit_vec3(origin_mm),
                emit_vec3(normal)
            );
            if let Some(x_axis) = x_axis {
                text.push_str(", x_axis = ");
                text.push_str(&emit_vec3(x_axis));
            }
            text.push(')');
            text
        }
    }
}

fn emit_sketch_dim(value: &SketchDimValue) -> String {
    match value {
        SketchDimValue::Literal(quantity) => format_quantity(*quantity),
        SketchDimValue::Param { param_id } => format!("param({})", format_binding(&param_id.0)),
    }
}

fn emit_entity(entity: &SketchEntity2D) -> String {
    match entity {
        SketchEntity2D::Point {
            id,
            position_mm,
            fixed,
            construction,
        } => {
            let mut text = format!("point({}, at = {}", emit_id(id), emit_vec2(position_mm));
            push_call_flag(&mut text, "fixed", *fixed);
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::Line {
            id,
            start_mm,
            end_mm,
            construction,
        } => {
            let mut text = format!(
                "line({}, start = {}, end = {}",
                emit_id(id),
                emit_vec2(start_mm),
                emit_vec2(end_mm)
            );
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::Arc {
            id,
            center_mm,
            start_mm,
            end_mm,
            ccw,
            construction,
        } => {
            let mut text = format!(
                "arc({}, center = {}, start = {}, end = {}",
                emit_id(id),
                emit_vec2(center_mm),
                emit_vec2(start_mm),
                emit_vec2(end_mm)
            );
            push_call_flag(&mut text, "ccw", *ccw);
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::Circle {
            id,
            center_mm,
            radius_mm,
            construction,
        } => {
            let mut text = format!(
                "circle({}, center = {}, radius = {}",
                emit_id(id),
                emit_vec2(center_mm),
                format_f64(*radius_mm)
            );
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::Rectangle {
            id,
            origin_mm,
            width_mm,
            height_mm,
            centered,
            construction,
        } => {
            let mut text = format!(
                "rectangle({}, origin = {}, width = {}, height = {}",
                emit_id(id),
                emit_vec2(origin_mm),
                format_f64(*width_mm),
                format_f64(*height_mm)
            );
            push_call_flag(&mut text, "centered", *centered);
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::SplineBlob {
            id,
            blob,
            closed,
            construction,
        } => {
            let mut text = format!(
                "spline({}, blob = {}",
                emit_id(id),
                format_blob_ref(&blob.0)
            );
            push_call_flag(&mut text, "closed", *closed);
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::Ellipse {
            id,
            center_mm,
            rx_mm,
            ry_mm,
            segments,
            construction,
        } => {
            let mut text = format!(
                "ellipse({}, center = {}, rx = {}, ry = {}",
                emit_id(id),
                emit_vec2(center_mm),
                format_f64(*rx_mm),
                format_f64(*ry_mm)
            );
            if let Some(segments) = segments {
                text.push_str(&format!(", segments = {segments}"));
            }
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
        SketchEntity2D::Spline {
            id,
            control_points_mm,
            closed,
            segments_per_span,
            construction,
        } => {
            let pts = render_inline(
                &control_points_mm
                    .iter()
                    .map(emit_vec2)
                    .collect::<Vec<_>>(),
            );
            let mut text = format!("spline({}, points = {pts}", emit_id(id));
            if let Some(segs) = segments_per_span {
                text.push_str(&format!(", segments = {segs}"));
            }
            push_call_flag(&mut text, "closed", *closed);
            push_call_flag(&mut text, "construction", *construction);
            text.push(')');
            text
        }
    }
}

fn push_call_flag(text: &mut String, key: &str, value: bool) {
    if value {
        text.push_str(", ");
        text.push_str(key);
        text.push_str(" = true");
    }
}

fn emit_profile(profile: &cad_ir::SketchProfileLoop) -> String {
    let ids = profile
        .entity_ids
        .iter()
        .map(|id| emit_id(id))
        .collect::<Vec<_>>()
        .join(", ");
    if profile.is_outer {
        format!(
            "loop({}, entities = [{ids}], outer = true)",
            emit_id(&profile.id)
        )
    } else {
        format!("loop({}, entities = [{ids}])", emit_id(&profile.id))
    }
}

fn emit_point_ref(point: &SketchPointRef) -> String {
    match point.anchor {
        SketchAnchor::Point => format!("point({})", emit_id(&point.entity_id)),
        other => format!(
            "anchor({}, {})",
            emit_id(&point.entity_id),
            anchor_name(other)
        ),
    }
}

fn emit_constraint(constraint: &SketchConstraint2D) -> String {
    match constraint {
        SketchConstraint2D::Coincident { name, a, b } => format!(
            "coincident({}a = {}, b = {})",
            name_prefix(name),
            emit_point_ref(a),
            emit_point_ref(b)
        ),
        SketchConstraint2D::PointOnLine { name, point, line } => format!(
            "point_on_line({}point = {}, line = {})",
            name_prefix(name),
            emit_point_ref(point),
            emit_id(line)
        ),
        SketchConstraint2D::Horizontal { name, entity }
        | SketchConstraint2D::Vertical { name, entity } => {
            let kind = match constraint {
                SketchConstraint2D::Horizontal { .. } => "horizontal",
                _ => "vertical",
            };
            format!("{kind}({}entity = {})", name_prefix(name), emit_id(entity))
        }
        SketchConstraint2D::Parallel { name, a, b }
        | SketchConstraint2D::Perpendicular { name, a, b }
        | SketchConstraint2D::Tangent { name, a, b }
        | SketchConstraint2D::Concentric { name, a, b }
        | SketchConstraint2D::Equal { name, a, b } => {
            let kind = match constraint {
                SketchConstraint2D::Parallel { .. } => "parallel",
                SketchConstraint2D::Perpendicular { .. } => "perpendicular",
                SketchConstraint2D::Tangent { .. } => "tangent",
                SketchConstraint2D::Concentric { .. } => "concentric",
                _ => "equal",
            };
            format!(
                "{kind}({}a = {}, b = {})",
                name_prefix(name),
                emit_id(a),
                emit_id(b)
            )
        }
        SketchConstraint2D::Symmetric {
            name,
            a,
            b,
            axis_line,
        } => format!(
            "symmetric({}a = {}, b = {}, axis = {})",
            name_prefix(name),
            emit_point_ref(a),
            emit_point_ref(b),
            emit_id(axis_line)
        ),
        SketchConstraint2D::Distance { name, a, b, value } => format!(
            "distance({}a = {}, b = {}, value = {})",
            name_prefix(name),
            emit_point_ref(a),
            emit_point_ref(b),
            emit_sketch_dim(value)
        ),
        SketchConstraint2D::Radius {
            name,
            entity,
            value,
        } => format!(
            "radius({}entity = {}, value = {})",
            name_prefix(name),
            emit_id(entity),
            emit_sketch_dim(value)
        ),
        SketchConstraint2D::Diameter {
            name,
            entity,
            value,
        } => format!(
            "diameter({}entity = {}, value = {})",
            name_prefix(name),
            emit_id(entity),
            emit_sketch_dim(value)
        ),
        SketchConstraint2D::Angle { name, a, b, value } => format!(
            "angle({}a = {}, b = {}, value = {})",
            name_prefix(name),
            emit_id(a),
            emit_id(b),
            emit_sketch_dim(value)
        ),
    }
}

fn name_prefix(name: &Option<String>) -> String {
    match name {
        Some(name) => format!("name = {}, ", format_string(name)),
        None => String::new(),
    }
}

fn emit_depth(depth: &HoleDepthSpec) -> Option<String> {
    match depth {
        HoleDepthSpec::ThroughAll => None,
        HoleDepthSpec::Blind { depth } => Some(format!("blind({})", emit_dim(depth))),
    }
}

fn emit_hole_profile(profile: &HoleProfileSpec) -> Option<String> {
    match profile {
        HoleProfileSpec::Simple => None,
        HoleProfileSpec::Counterbore {
            bore_diameter,
            bore_depth,
        } => Some(format!(
            "counterbore(diameter = {}, depth = {})",
            emit_dim(bore_diameter),
            emit_dim(bore_depth)
        )),
        HoleProfileSpec::Countersink {
            sink_diameter,
            sink_angle,
        } => Some(format!(
            "countersink(diameter = {}, angle = {})",
            emit_dim(sink_diameter),
            emit_dim(sink_angle)
        )),
    }
}

fn emit_pattern_axis(axis: &LinearPatternAxisSpec) -> String {
    format!(
        "axis(direction = {}, count = {}, spacing = {})",
        emit_vec3(&axis.direction),
        emit_int(&axis.count),
        emit_dim(&axis.spacing)
    )
}

fn emit_sweep_path(path: &SweepPathSpec) -> String {
    match path {
        SweepPathSpec::Sketch { node_id } => emit_node(node_id),
        SweepPathSpec::Polyline { points_mm } => render_inline(
            &points_mm
                .iter()
                .map(|point| emit_vec3(point))
                .collect::<Vec<_>>(),
        ),
    }
}

fn emit_mate_kind(kind: &MateKindSpec) -> String {
    match kind {
        MateKindSpec::Coincident => "coincident".to_owned(),
        MateKindSpec::Concentric => "concentric".to_owned(),
        MateKindSpec::Parallel => "parallel".to_owned(),
        MateKindSpec::Perpendicular => "perpendicular".to_owned(),
        MateKindSpec::Distance { value } => format!("distance({})", emit_dim(value)),
        MateKindSpec::Angle { value } => format!("angle({})", emit_dim(value)),
        MateKindSpec::Gear { ratio } => format!("gear({})", format_scalar(*ratio)),
    }
}

fn emit_rule(kind: &DesignAssertionKind) -> String {
    match kind {
        DesignAssertionKind::VolumeBounds { min_mm3, max_mm3 } => {
            let mut parts = Vec::new();
            if let Some(min) = min_mm3 {
                parts.push(format!("min = {}", format_scalar(*min)));
            }
            if let Some(max) = max_mm3 {
                parts.push(format!("max = {}", format_scalar(*max)));
            }
            format!("volume({})", parts.join(", "))
        }
        DesignAssertionKind::MinWallThickness { min } => format!("min_wall({})", emit_dim(min)),
        DesignAssertionKind::BooleanCondition { condition } => {
            format!("condition({})", emit_node(condition))
        }
        DesignAssertionKind::Custom { expression, params } => {
            if params.is_empty() {
                format!("custom({})", format_string(expression))
            } else {
                let pairs = params
                    .iter()
                    .map(|(key, value)| format!("{} = {}", format_key(key), format_string(value)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "custom({}, params = {{ {pairs} }})",
                    format_string(expression)
                )
            }
        }
    }
}

fn datum_name(plane: cad_ir::DatumPlane) -> &'static str {
    match plane {
        cad_ir::DatumPlane::Xy => "xy",
        cad_ir::DatumPlane::Xz => "xz",
        cad_ir::DatumPlane::Yz => "yz",
    }
}

fn anchor_name(anchor: SketchAnchor) -> &'static str {
    match anchor {
        SketchAnchor::Point => "point",
        SketchAnchor::Start => "start",
        SketchAnchor::End => "end",
        SketchAnchor::Center => "center",
        SketchAnchor::Midpoint => "midpoint",
    }
}

fn body_op_name(op: FeatureBodyOp) -> &'static str {
    match op {
        FeatureBodyOp::NewBody => "new_body",
        FeatureBodyOp::Join => "join",
        FeatureBodyOp::Cut => "cut",
        FeatureBodyOp::Intersect => "intersect",
    }
}

fn fastener_name(standard: cad_ir::FastenerHoleStandard) -> &'static str {
    match standard {
        cad_ir::FastenerHoleStandard::Iso4762Counterbore => "iso4762_counterbore",
        cad_ir::FastenerHoleStandard::Iso10642Countersink => "iso10642_countersink",
        cad_ir::FastenerHoleStandard::MetricTapDrill => "metric_tap_drill",
        cad_ir::FastenerHoleStandard::MetricClearance => "metric_clearance",
    }
}

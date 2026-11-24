//! Graph walks shared by the compiler and the read APIs.
//!
//! The design contract lives on a reserved assertion node because
//! [`cad_ir::CanonicalGraph`] has no document-level metadata field. Feature
//! tree order is the `outputs` list, which the compiler rewrites with
//! `SetOutputs`.

use cad_ir::{
    CanonicalGraph, ComponentSourceSpec, DesignAssertionKind, DimInput, DirectFaceEditOp,
    FeatureSpec, HoleDepthSpec, HoleProfileSpec, IntInput, IrNode, IrValue, MateKindSpec, NodeId,
    NodeState, SketchConstraint2D, SketchPlaneSpec,
};

/// Reserved node that stores [`crate::DesignContract`] as a custom design assertion.
pub(crate) const CONTRACT_NODE_ID: &str = "__design_contract";

/// Metadata key holding the JSON catalog row for a standard fastener hole.
pub(crate) const STDLIB_META_KEY: &str = "stdlib_fastener";

pub(crate) fn is_outline_feature(graph: &CanonicalGraph, id: &NodeId) -> bool {
    if id.0 == CONTRACT_NODE_ID {
        return false;
    }
    graph
        .nodes
        .get(&id.0)
        .is_some_and(|node| node.try_feature_spec().is_ok())
}

/// Feature tree order: explicit `outputs` first, then any feature not listed, by id.
pub(crate) fn read_outline(graph: &CanonicalGraph) -> Vec<NodeId> {
    let mut outline = Vec::new();
    for id in &graph.outputs {
        if is_outline_feature(graph, id) && !outline.contains(id) {
            outline.push(id.clone());
        }
    }
    let mut extras: Vec<NodeId> = graph
        .nodes
        .keys()
        .filter_map(|key| {
            let id = NodeId::from(key.as_str());
            if is_outline_feature(graph, &id) && !outline.contains(&id) {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    extras.sort();
    outline.extend(extras);
    outline
}

/// `outputs` payload for a patch: the feature outline, then non-feature outputs.
pub(crate) fn publish_outputs(graph: &CanonicalGraph, outline: &[NodeId]) -> Vec<NodeId> {
    let mut outputs = Vec::new();
    for id in outline {
        if id.0 != CONTRACT_NODE_ID && !outputs.contains(id) {
            outputs.push(id.clone());
        }
    }
    for id in &graph.outputs {
        if id.0 == CONTRACT_NODE_ID || outputs.contains(id) {
            continue;
        }
        if graph.nodes.contains_key(&id.0) && !is_outline_feature(graph, id) {
            outputs.push(id.clone());
        }
    }
    outputs
}

pub(crate) fn feature_label(spec: &FeatureSpec) -> &'static str {
    match spec {
        FeatureSpec::PrimitiveBox { .. } => "primitive_box",
        FeatureSpec::PrimitiveCylinder { .. } => "primitive_cylinder",
        FeatureSpec::Sketch2D(_) => "sketch_2d",
        FeatureSpec::Extrude { .. } => "extrude",
        FeatureSpec::Revolve { .. } => "revolve",
        FeatureSpec::Sweep { .. } => "sweep",
        FeatureSpec::Loft { .. } => "loft",
        FeatureSpec::Hole { .. } => "hole",
        FeatureSpec::StdFastenerHole { .. } => "std_fastener_hole",
        FeatureSpec::Fillet { .. } => "fillet",
        FeatureSpec::Chamfer { .. } => "chamfer",
        FeatureSpec::Shell { .. } => "shell",
        FeatureSpec::Draft { .. } => "draft",
        FeatureSpec::LinearPattern { .. } => "linear_pattern",
        FeatureSpec::PolarPattern { .. } => "polar_pattern",
        FeatureSpec::Mirror { .. } => "mirror",
        FeatureSpec::Boolean { .. } => "boolean",
        FeatureSpec::DirectFaceEdit { .. } => "direct_face_edit",
        FeatureSpec::ImportStep { .. } => "import_step",
        FeatureSpec::ComponentInstance { .. } => "component_instance",
        FeatureSpec::MateConstraint { .. } => "mate_constraint",
        FeatureSpec::TagTopology { .. } => "tag_topology",
        FeatureSpec::DesignAssertion { .. } => "design_assertion",
    }
}

pub(crate) fn is_suppressed(node: &IrNode) -> bool {
    matches!(node.state, NodeState::Suppressed)
}

fn push_unique(refs: &mut Vec<NodeId>, id: &NodeId) {
    if !refs.contains(id) {
        refs.push(id.clone());
    }
}

fn push_dim(refs: &mut Vec<NodeId>, dim: &DimInput) {
    if let DimInput::NodeOutput { node_id, .. } = dim {
        push_unique(refs, node_id);
    }
}

fn push_int(refs: &mut Vec<NodeId>, input: &IntInput) {
    if let IntInput::NodeOutput { node_id, .. } = input {
        push_unique(refs, node_id);
    }
}

fn push_depth(refs: &mut Vec<NodeId>, depth: &HoleDepthSpec) {
    if let HoleDepthSpec::Blind { depth } = depth {
        push_dim(refs, depth);
    }
}

fn push_profile(refs: &mut Vec<NodeId>, profile: &HoleProfileSpec) {
    match profile {
        HoleProfileSpec::Simple => {}
        HoleProfileSpec::Counterbore {
            bore_diameter,
            bore_depth,
        } => {
            push_dim(refs, bore_diameter);
            push_dim(refs, bore_depth);
        }
        HoleProfileSpec::Countersink {
            sink_diameter,
            sink_angle,
        } => {
            push_dim(refs, sink_diameter);
            push_dim(refs, sink_angle);
        }
    }
}

fn push_mate(refs: &mut Vec<NodeId>, mate: &MateKindSpec) {
    match mate {
        MateKindSpec::Distance { value } | MateKindSpec::Angle { value } => push_dim(refs, value),
        _ => {}
    }
}

/// Direct node inputs of a feature, in argument order.
pub(crate) fn spec_node_refs(spec: &FeatureSpec) -> Vec<NodeId> {
    let mut refs = Vec::new();
    match spec {
        FeatureSpec::PrimitiveBox {
            width,
            depth,
            height,
            ..
        } => {
            push_dim(&mut refs, width);
            push_dim(&mut refs, depth);
            push_dim(&mut refs, height);
        }
        FeatureSpec::PrimitiveCylinder { radius, height, .. } => {
            push_dim(&mut refs, radius);
            push_dim(&mut refs, height);
        }
        FeatureSpec::Sketch2D(_) => {}
        FeatureSpec::Extrude {
            sketch,
            distance,
            target,
            ..
        } => {
            push_unique(&mut refs, sketch);
            push_dim(&mut refs, distance);
            if let Some(target) = target {
                push_unique(&mut refs, target);
            }
        }
        FeatureSpec::Revolve {
            sketch,
            angle,
            target,
            ..
        } => {
            push_unique(&mut refs, sketch);
            push_dim(&mut refs, angle);
            if let Some(target) = target {
                push_unique(&mut refs, target);
            }
        }
        FeatureSpec::Sweep { profile, path, .. } => {
            push_unique(&mut refs, profile);
            if let cad_ir::SweepPathSpec::Sketch { node_id } = path {
                push_unique(&mut refs, node_id);
            }
        }
        FeatureSpec::Loft {
            sections, guides, ..
        } => {
            for id in sections.iter().chain(guides.iter()) {
                push_unique(&mut refs, id);
            }
        }
        FeatureSpec::Hole {
            solid,
            diameter,
            depth,
            profile,
            ..
        } => {
            push_unique(&mut refs, solid);
            push_dim(&mut refs, diameter);
            push_depth(&mut refs, depth);
            push_profile(&mut refs, profile);
        }
        FeatureSpec::StdFastenerHole {
            solid,
            depth,
            thread_depth,
            ..
        } => {
            push_unique(&mut refs, solid);
            push_depth(&mut refs, depth);
            if let Some(thread_depth) = thread_depth {
                push_dim(&mut refs, thread_depth);
            }
        }
        FeatureSpec::Fillet { solid, radius, .. } => {
            push_unique(&mut refs, solid);
            push_dim(&mut refs, radius);
        }
        FeatureSpec::Chamfer {
            solid,
            distance,
            distance2,
            angle,
            ..
        } => {
            push_unique(&mut refs, solid);
            push_dim(&mut refs, distance);
            if let Some(distance2) = distance2 {
                push_dim(&mut refs, distance2);
            }
            if let Some(angle) = angle {
                push_dim(&mut refs, angle);
            }
        }
        FeatureSpec::Shell {
            solid, thickness, ..
        } => {
            push_unique(&mut refs, solid);
            push_dim(&mut refs, thickness);
        }
        FeatureSpec::Draft { solid, angle, .. } => {
            push_unique(&mut refs, solid);
            push_dim(&mut refs, angle);
        }
        FeatureSpec::LinearPattern {
            solid,
            count,
            spacing,
            second_direction,
            ..
        } => {
            push_unique(&mut refs, solid);
            push_int(&mut refs, count);
            push_dim(&mut refs, spacing);
            if let Some(axis) = second_direction {
                push_int(&mut refs, &axis.count);
                push_dim(&mut refs, &axis.spacing);
            }
        }
        FeatureSpec::PolarPattern {
            solid,
            count,
            total_angle,
            ..
        } => {
            push_unique(&mut refs, solid);
            push_int(&mut refs, count);
            push_dim(&mut refs, total_angle);
        }
        FeatureSpec::Mirror { solid, .. } => push_unique(&mut refs, solid),
        FeatureSpec::Boolean { target, tools, .. } => {
            push_unique(&mut refs, target);
            for tool in tools {
                push_unique(&mut refs, tool);
            }
        }
        FeatureSpec::DirectFaceEdit { solid, edit, .. } => {
            push_unique(&mut refs, solid);
            if let DirectFaceEditOp::Offset { distance } = edit {
                push_dim(&mut refs, distance);
            }
        }
        FeatureSpec::ImportStep { .. } => {}
        FeatureSpec::ComponentInstance { source, .. } => {
            if let ComponentSourceSpec::LocalNode { node_id } = source {
                push_unique(&mut refs, node_id);
            }
        }
        FeatureSpec::MateConstraint {
            left,
            right,
            mate_kind,
            ..
        } => {
            push_unique(&mut refs, left);
            push_unique(&mut refs, right);
            push_mate(&mut refs, mate_kind);
        }
        FeatureSpec::TagTopology { solid, .. } => push_unique(&mut refs, solid),
        FeatureSpec::DesignAssertion { target, kind, .. } => {
            if let Some(target) = target {
                push_unique(&mut refs, target);
            }
            match kind {
                DesignAssertionKind::MinWallThickness { min } => push_dim(&mut refs, min),
                DesignAssertionKind::BooleanCondition { condition } => {
                    push_unique(&mut refs, condition);
                }
                _ => {}
            }
        }
    }
    refs
}

fn push_topo(out: &mut Vec<String>, value: &str) {
    if !value.trim().is_empty() && !out.iter().any(|existing| existing == value) {
        out.push(value.to_owned());
    }
}

fn collect_plane(out: &mut Vec<String>, plane: &SketchPlaneSpec) {
    if let SketchPlaneSpec::FaceRef { face, .. } = plane {
        push_topo(out, face);
    }
}

/// Topology selectors attached to a feature, including sketch face references.
pub(crate) fn spec_topo_refs(spec: &FeatureSpec) -> Vec<String> {
    let mut out = Vec::new();
    match spec {
        FeatureSpec::Sketch2D(sketch) => collect_plane(&mut out, &sketch.plane),
        FeatureSpec::Hole { target_face, .. }
        | FeatureSpec::StdFastenerHole { target_face, .. } => {
            if let Some(face) = target_face {
                push_topo(&mut out, face);
            }
        }
        FeatureSpec::Fillet { edges, .. } | FeatureSpec::Chamfer { edges, .. } => {
            for edge in edges {
                push_topo(&mut out, edge);
            }
        }
        FeatureSpec::Shell { open_faces, .. } => {
            for face in open_faces {
                push_topo(&mut out, face);
            }
        }
        FeatureSpec::Draft {
            faces,
            neutral_plane,
            ..
        } => {
            for face in faces {
                push_topo(&mut out, face);
            }
            collect_plane(&mut out, neutral_plane);
        }
        FeatureSpec::Mirror { plane, .. } => collect_plane(&mut out, plane),
        FeatureSpec::DirectFaceEdit { face, .. } => push_topo(&mut out, face),
        FeatureSpec::MateConstraint {
            left_ref,
            right_ref,
            ..
        } => {
            push_topo(&mut out, left_ref);
            push_topo(&mut out, right_ref);
        }
        FeatureSpec::TagTopology { targets, .. } => {
            for target in targets {
                push_topo(&mut out, target);
            }
        }
        _ => {}
    }
    out
}

pub(crate) fn node_upstream(node: &IrNode) -> Vec<NodeId> {
    if let Ok(spec) = node.try_feature_spec() {
        return spec_node_refs(&spec);
    }
    let mut refs = Vec::new();
    collect_value_nodes(
        &node.inputs.values().cloned().collect::<Vec<_>>(),
        &mut refs,
    );
    refs
}

fn collect_value_nodes(values: &[IrValue], refs: &mut Vec<NodeId>) {
    for value in values {
        match value {
            IrValue::NodeRef { node_id, .. } => push_unique(refs, node_id),
            IrValue::List(items) => collect_value_nodes(items, refs),
            IrValue::Map(entries) => {
                let nested: Vec<IrValue> = entries.values().cloned().collect();
                collect_value_nodes(&nested, refs);
            }
            _ => {}
        }
    }
}

pub(crate) fn node_topo_refs(node: &IrNode) -> Vec<String> {
    if let Ok(spec) = node.try_feature_spec() {
        return spec_topo_refs(&spec);
    }
    let mut out = Vec::new();
    for value in node.inputs.values() {
        collect_value_topo(value, &mut out);
    }
    out
}

fn collect_value_topo(value: &IrValue, out: &mut Vec<String>) {
    match value {
        IrValue::TopoRefExpr(topo) => push_topo(out, topo),
        IrValue::List(items) => {
            for item in items {
                collect_value_topo(item, out);
            }
        }
        IrValue::Map(entries) => {
            for item in entries.values() {
                collect_value_topo(item, out);
            }
        }
        _ => {}
    }
}

/// Other nodes whose inputs still point at `id`.
pub(crate) fn node_users(graph: &CanonicalGraph, id: &NodeId) -> Vec<NodeId> {
    let mut users = Vec::new();
    for (other_id, node) in &graph.nodes {
        if other_id == &id.0 {
            continue;
        }
        if node_upstream(node).iter().any(|upstream| upstream == id) {
            users.push(node.id.clone());
        }
    }
    users
}

pub(crate) fn parameter_users(graph: &CanonicalGraph, name: &str) -> Vec<String> {
    let mut users = Vec::new();
    for (id, def) in &graph.parameters {
        if id == name {
            continue;
        }
        let Some(formula) = def.formula.as_deref() else {
            continue;
        };
        if let Ok(deps) = cad_ir::collect_formula_dependencies(&def.name, formula)
            && deps.iter().any(|dep| dep.0 == name)
        {
            users.push(format!("parameter `{id}`"));
        }
    }
    for (id, node) in &graph.nodes {
        if let Ok(spec) = node.try_feature_spec()
            && spec.referenced_params().iter().any(|param| param.0 == name)
        {
            users.push(format!("node `{id}`"));
        }
    }
    users
}

pub(crate) fn constraint_name(constraint: &SketchConstraint2D) -> Option<&str> {
    let name = match constraint {
        SketchConstraint2D::Coincident { name, .. }
        | SketchConstraint2D::PointOnLine { name, .. }
        | SketchConstraint2D::Horizontal { name, .. }
        | SketchConstraint2D::Vertical { name, .. }
        | SketchConstraint2D::Parallel { name, .. }
        | SketchConstraint2D::Perpendicular { name, .. }
        | SketchConstraint2D::Tangent { name, .. }
        | SketchConstraint2D::Concentric { name, .. }
        | SketchConstraint2D::Equal { name, .. }
        | SketchConstraint2D::Symmetric { name, .. }
        | SketchConstraint2D::Distance { name, .. }
        | SketchConstraint2D::Radius { name, .. }
        | SketchConstraint2D::Diameter { name, .. }
        | SketchConstraint2D::Angle { name, .. } => name.as_deref(),
    };
    name.filter(|value| !value.trim().is_empty())
}

/// Features that do not feed another feature, in outline order.
pub(crate) fn leaf_outputs(graph: &CanonicalGraph, outline: &[NodeId]) -> Vec<NodeId> {
    outline
        .iter()
        .filter(|id| {
            !graph.nodes.values().any(|node| {
                node.id != **id && node_upstream(node).iter().any(|upstream| upstream == *id)
            })
        })
        .cloned()
        .collect()
}

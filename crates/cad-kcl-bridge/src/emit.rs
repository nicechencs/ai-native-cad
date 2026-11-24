//! Deterministic `CanonicalGraph` -> `.kcl 2.0` projection.

use std::collections::BTreeMap;

use cad_ir::{
    CanonicalGraph, ExternalRefDef, IrNode, IrValue, NodeId, NodeKind, NodeState, OUTPUT_PORT,
    UnitSystem, validate_graph,
};

use crate::BridgeError;
use crate::model::{
    angle_unit_name, blob_kind_name, force_unit_name, format_binding, format_blob_ref,
    format_external_ref, format_f64, format_key, format_node_ref, format_quantity, format_scalar,
    format_string, length_unit_name, mass_unit_name, operator_name, payload_policy_name,
    pressure_unit_name, type_name,
};

/// Project `graph` to a formatted `.kcl 2.0` view.
///
/// Equal graphs produce byte-identical text. Node bindings follow the validated
/// topological order (ties broken by node id). Parameters, blobs, external
/// references, and input ports follow their ordered map keys. Output order is
/// the graph's output list.
pub fn emit_kcl(graph: &CanonicalGraph) -> Result<String, BridgeError> {
    let order = validate_graph(graph).map_err(|err| BridgeError::at(1, 1, err.to_string()))?;
    Ok(render(graph, &order))
}

fn render(graph: &CanonicalGraph, order: &[NodeId]) -> String {
    let mut out = String::new();
    push_line(
        &mut out,
        &format!("@ir_schema({})", graph.ir_schema_version),
    );
    push_line(
        &mut out,
        &format!("@graph_id({})", format_string(&graph.graph_id)),
    );
    push_line(&mut out, &format_units(&graph.units));
    out.push('\n');

    if !graph.blobs.is_empty() {
        for blob in graph.blobs.values() {
            push_line(
                &mut out,
                &format!(
                    "@blob({}, kind = {}, sha256 = {}, path = {}, bytes = {})",
                    format_string(&blob.handle.0),
                    format_string(blob_kind_name(blob.kind)),
                    format_string(&blob.sha256),
                    format_string(&blob.relative_path),
                    blob.byte_len,
                ),
            );
        }
        out.push('\n');
    }

    if !graph.external_refs.is_empty() {
        for external_ref in graph.external_refs.values() {
            push_line(&mut out, &format_external_ref_def(external_ref));
        }
        out.push('\n');
    }

    if !graph.parameters.is_empty() {
        for parameter in graph.parameters.values() {
            let mut line = format!(
                "param {}: {} = {}",
                format_binding(&parameter.name.0),
                type_name(parameter.ir_type),
                format_value(&parameter.value),
            );
            if let Some(formula) = &parameter.formula {
                line.push_str(" @formula(");
                line.push_str(&format_string(formula));
                line.push(')');
            }
            push_line(&mut out, &line);
        }
        out.push('\n');
    }

    for (index, node_id) in order.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let node = &graph.nodes[&node_id.0];
        emit_node(&mut out, node);
    }
    if !order.is_empty() {
        out.push('\n');
    }

    let outputs = graph
        .outputs
        .iter()
        .map(|id| format_node_ref(&id.0))
        .collect::<Vec<_>>()
        .join(", ");
    push_line(&mut out, &format!("return [{outputs}]"));
    out
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn format_units(units: &UnitSystem) -> String {
    format!(
        "@unit(length = {}, angle = {}, force = {}, pressure = {}, mass = {})",
        length_unit_name(units.length),
        angle_unit_name(units.angle),
        force_unit_name(units.force),
        pressure_unit_name(units.pressure),
        mass_unit_name(units.mass),
    )
}

fn format_external_ref_def(external_ref: &ExternalRefDef) -> String {
    let mut parts = vec![
        format_string(&external_ref.ref_id.0),
        format!("path = {}", format_string(&external_ref.container_path)),
        format!(
            "policy = {}",
            format_string(payload_policy_name(external_ref.payload_policy))
        ),
    ];
    if let Some(sha256) = &external_ref.expected_sha256 {
        parts.push(format!("sha256 = {}", format_string(sha256)));
    }
    if let Some(target_graph) = &external_ref.target_graph_id {
        parts.push(format!("target_graph = {}", format_string(target_graph)));
    }
    if !external_ref.overrides.is_empty() {
        parts.push(format!(
            "overrides = {}",
            format_map(&external_ref.overrides)
        ));
    }
    format!("@extref({})", parts.join(", "))
}

fn emit_node(out: &mut String, node: &IrNode) {
    match &node.state {
        NodeState::Valid => {}
        NodeState::Suppressed => push_line(out, "@suppressed"),
        NodeState::Stale => push_line(out, "@stale"),
        NodeState::FailedWithRollback {
            reason,
            failed_topo_refs,
        } => {
            if failed_topo_refs.is_empty() {
                push_line(out, &format!("@failed({})", format_string(reason)));
            } else {
                let refs = failed_topo_refs
                    .iter()
                    .map(|item| format_string(item))
                    .collect::<Vec<_>>()
                    .join(", ");
                push_line(
                    out,
                    &format!("@failed({}, refs = [{refs}])", format_string(reason)),
                );
            }
        }
    }
    if node.name != node.id.0 {
        push_line(out, &format!("@name({})", format_string(&node.name)));
    }
    if let Some(kernel) = &node.kernel_op_version {
        push_line(out, &format!("@kernel({})", format_string(kernel)));
    }
    if !node.metadata.is_empty() {
        let pairs = node
            .metadata
            .iter()
            .map(|(key, value)| format!("{} = {}", format_key(key), format_string(value)))
            .collect::<Vec<_>>()
            .join(", ");
        push_line(out, &format!("@meta({pairs})"));
    }

    if let Some(statement) = crate::feature_kcl::try_emit_feature(node) {
        push_line(out, &statement);
        return;
    }

    let binding = format_binding(&node.id.0);
    let operator = operator_name(&node.kind);
    let head = format!("let {binding} = {operator}");
    let ty = type_name(node.output_type);
    let positional = match &node.kind {
        NodeKind::ParameterRef { param_id } => Some(format_string(&param_id.0)),
        _ => None,
    };
    push_line(
        out,
        &format_call(&head, positional.as_deref(), &node.inputs, ty),
    );
}

fn format_call(
    head: &str,
    positional: Option<&str>,
    inputs: &BTreeMap<String, IrValue>,
    ty: &str,
) -> String {
    if positional.is_none() && inputs.is_empty() {
        return format!("{head}() -> {ty}");
    }
    if inputs.is_empty()
        && let Some(positional) = positional
    {
        return format!("{head}({positional}) -> {ty}");
    }

    let mut lines = vec![format!("{head}(")];
    if let Some(positional) = positional {
        lines.push(format!("    {positional},"));
    }
    for (key, value) in inputs {
        lines.push(format!(
            "    {} = {},",
            format_key(key),
            format_value(value)
        ));
    }
    lines.push(format!(") -> {ty}"));
    lines.join("\n")
}

fn format_value(value: &IrValue) -> String {
    match value {
        IrValue::Quantity(quantity) => format_quantity(*quantity),
        IrValue::Scalar(number) => format_scalar(*number),
        IrValue::Int(number) => number.to_string(),
        IrValue::Bool(true) => "true".to_owned(),
        IrValue::Bool(false) => "false".to_owned(),
        IrValue::String(text) => format_string(text),
        IrValue::Vec3([x, y, z]) => {
            format!(
                "vec3({}, {}, {})",
                format_f64(*x),
                format_f64(*y),
                format_f64(*z)
            )
        }
        IrValue::NodeRef { node_id, port } => {
            debug_assert_eq!(port, OUTPUT_PORT);
            format_node_ref(&node_id.0)
        }
        IrValue::BlobRef(handle) => format_blob_ref(&handle.0),
        IrValue::ExternalRef(ref_id) => format_external_ref(&ref_id.0),
        IrValue::TopoRefExpr(expr) => format!("@topo({})", format_string(expr)),
        IrValue::List(items) => {
            let inner = items
                .iter()
                .map(format_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        IrValue::Map(entries) => format_map(entries),
    }
}

fn format_map(entries: &BTreeMap<String, IrValue>) -> String {
    if entries.is_empty() {
        "{}".to_owned()
    } else {
        let inner = entries
            .iter()
            .map(|(key, value)| format!("{} = {}", format_key(key), format_value(value)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{ {inner} }}")
    }
}

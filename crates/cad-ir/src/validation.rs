use std::collections::{BTreeMap, BTreeSet};

use crate::{
    CanonicalGraph, FEATURE_SPEC_PORT, IrError, IrType, IrValue, NodeId, NodeKind, OUTPUT_PORT,
    ParamId, formula::resolve_formula_values, ports,
};

/// Check identities, references, and built-in port types, then return the
/// topological execution order. Ties are resolved by lexical node ID.
pub fn validate_graph(graph: &CanonicalGraph) -> Result<Vec<NodeId>, IrError> {
    check_id("graph", &graph.graph_id)?;
    check_table("parameter", &graph.parameters, |entry| &entry.name.0)?;
    check_table("blob", &graph.blobs, |entry| &entry.handle.0)?;
    check_table("external_ref", &graph.external_refs, |entry| {
        &entry.ref_id.0
    })?;
    check_table("node", &graph.nodes, |entry| &entry.id.0)?;

    let ext_node_deps = validate_external_refs(graph)?;

    for parameter in graph.parameters.values() {
        let mut dependencies = BTreeSet::new();
        let location = format!("parameter `{}`", parameter.name);
        let value = &parameter.value;
        if check_value(graph, value, &ext_node_deps, &mut dependencies)? || !dependencies.is_empty()
        {
            return Err(IrError::NonLiteralParameter(parameter.name.clone()));
        }
        check_value_type(
            location,
            value,
            &[parameter.ir_type],
            value_type(graph, value),
        )?;
    }

    // Validate all parameter formulas (syntax, references, acyclicity, dimensional algebra).
    resolve_formula_values(graph)?;

    let mut outputs = BTreeSet::new();
    for output in &graph.outputs {
        check_id("node", &output.0)?;
        if !outputs.insert(output) {
            return Err(duplicate_id("output", &output.0));
        }
        if !graph.nodes.contains_key(&output.0) {
            return Err(IrError::DanglingNode(output.clone()));
        }
    }

    let mut incoming = BTreeMap::new();
    let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
    for (id, node) in &graph.nodes {
        if let NodeKind::ParameterRef { param_id } = &node.kind {
            let parameter = find_parameter(graph, param_id)?;
            check_output_type(id, &[parameter.ir_type], node.output_type)?;
        } else if let Some(types) = ports::output_types(&node.kind) {
            check_output_type(id, types, node.output_type)?;
        }

        let mut dependencies = BTreeSet::new();
        for (port, value) in &node.inputs {
            let has_connection = check_value(graph, value, &ext_node_deps, &mut dependencies)?;
            let expected = ports::input_type(&node.kind, port);
            if let Some(expected) = expected {
                check_input_type(graph, id, port, value, expected)?;
            } else if has_connection && !matches!(node.kind, NodeKind::StdLibCall) {
                return Err(IrError::UnknownInputPort {
                    node_id: node.id.clone(),
                    port: port.clone(),
                });
            }
        }
        for names in ports::required_inputs(&node.kind) {
            if !names.iter().any(|name| node.inputs.contains_key(*name)) {
                return Err(IrError::MissingInput {
                    node_id: node.id.clone(),
                    port: names[0],
                });
            }
        }
        if node.inputs.contains_key(FEATURE_SPEC_PORT) {
            let spec = node.try_feature_spec()?;
            for param_id in spec.referenced_params() {
                find_parameter(graph, &param_id)?;
            }
        }
        incoming.insert(id.as_str(), dependencies.len());
        for dependency in dependencies {
            dependents.entry(dependency).or_default().push(id);
        }
    }

    execution_order(incoming, dependents)
}

fn is_valid_sha256_hex(sha256: &str) -> bool {
    sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_external_refs(
    graph: &CanonicalGraph,
) -> Result<BTreeMap<&str, BTreeSet<&str>>, IrError> {
    let mut direct_node_deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut direct_ext_deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();

    for (id, external_ref) in &graph.external_refs {
        check_id("container_path", &external_ref.container_path)?;
        if let Some(target_graph_id) = &external_ref.target_graph_id {
            check_id("target_graph", target_graph_id)?;
        }
        if let Some(sha256) = &external_ref.expected_sha256
            && !is_valid_sha256_hex(sha256)
        {
            return Err(IrError::InvalidSha256 {
                entity: "external_ref",
                id: id.clone(),
                sha256: sha256.clone(),
            });
        }

        let mut node_deps = BTreeSet::new();
        let mut ext_deps = BTreeSet::new();
        for (override_key, override_val) in &external_ref.overrides {
            check_id("override", override_key)?;
            check_override_value(graph, override_val, &mut node_deps, &mut ext_deps)?;
        }
        direct_node_deps.insert(id.as_str(), node_deps);
        direct_ext_deps.insert(id.as_str(), ext_deps);
    }

    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    let mut resolved: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for id in graph.external_refs.keys() {
        resolve_ext_deps(
            id.as_str(),
            &direct_node_deps,
            &direct_ext_deps,
            &mut state,
            &mut resolved,
        )?;
    }
    Ok(resolved)
}

fn resolve_ext_deps<'a>(
    id: &'a str,
    direct_node_deps: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    direct_ext_deps: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    state: &mut BTreeMap<&'a str, u8>,
    resolved: &mut BTreeMap<&'a str, BTreeSet<&'a str>>,
) -> Result<BTreeSet<&'a str>, IrError> {
    match state.get(id).copied() {
        Some(1) => return Err(IrError::CycleDetected),
        Some(2) => return Ok(resolved[id].clone()),
        _ => {}
    }
    state.insert(id, 1);
    let mut nodes = direct_node_deps.get(id).cloned().unwrap_or_default();
    if let Some(children) = direct_ext_deps.get(id) {
        for &child in children {
            let child_nodes =
                resolve_ext_deps(child, direct_node_deps, direct_ext_deps, state, resolved)?;
            nodes.extend(child_nodes);
        }
    }
    state.insert(id, 2);
    resolved.insert(id, nodes.clone());
    Ok(nodes)
}

fn check_override_value<'a>(
    graph: &CanonicalGraph,
    value: &'a IrValue,
    node_deps: &mut BTreeSet<&'a str>,
    ext_deps: &mut BTreeSet<&'a str>,
) -> Result<(), IrError> {
    match value {
        IrValue::Quantity(quantity) if !quantity.canonical_value().is_finite() => {
            return Err(IrError::NonFiniteNumber);
        }
        IrValue::Scalar(value) if !value.is_finite() => return Err(IrError::NonFiniteNumber),
        IrValue::Vec3(values) if !values.iter().all(|value| value.is_finite()) => {
            return Err(IrError::NonFiniteNumber);
        }
        IrValue::NodeRef { node_id, port } => {
            check_id("node", &node_id.0)?;
            if !graph.nodes.contains_key(&node_id.0) {
                return Err(IrError::DanglingNode(node_id.clone()));
            }
            if port != OUTPUT_PORT {
                return Err(IrError::UnknownOutputPort {
                    node_id: node_id.clone(),
                    port: port.clone(),
                });
            }
            node_deps.insert(&node_id.0);
        }
        IrValue::BlobRef(handle) => {
            check_id("blob", &handle.0)?;
            if !graph.blobs.contains_key(&handle.0) {
                return Err(IrError::DanglingBlob(handle.clone()));
            }
        }
        IrValue::ExternalRef(ref_id) => {
            check_id("external_ref", &ref_id.0)?;
            if !graph.external_refs.contains_key(&ref_id.0) {
                return Err(IrError::DanglingExternalRef(ref_id.clone()));
            }
            ext_deps.insert(&ref_id.0);
        }
        IrValue::List(values) => {
            for value in values {
                check_override_value(graph, value, node_deps, ext_deps)?;
            }
        }
        IrValue::Map(values) => {
            for value in values.values() {
                check_override_value(graph, value, node_deps, ext_deps)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_id(entity: &'static str, id: &str) -> Result<(), IrError> {
    if id.trim().is_empty() {
        return Err(IrError::EmptyId { entity });
    }
    Ok(())
}

fn duplicate_id(entity: &'static str, id: &str) -> IrError {
    IrError::DuplicateId {
        entity,
        id: id.to_owned(),
    }
}

fn check_table<T>(
    entity: &'static str,
    entries: &BTreeMap<String, T>,
    entry_id: impl Fn(&T) -> &str,
) -> Result<(), IrError> {
    let mut ids = BTreeSet::new();
    for (key, entry) in entries {
        let id = entry_id(entry);
        check_id(entity, key)?;
        check_id(entity, id)?;
        if !ids.insert(id) {
            return Err(duplicate_id(entity, id));
        }
    }
    for (key, entry) in entries {
        let id = entry_id(entry);
        if key != id {
            return Err(IrError::IdMismatch {
                entity,
                key: key.clone(),
                id: id.to_owned(),
            });
        }
    }
    Ok(())
}

fn find_parameter<'a>(
    graph: &'a CanonicalGraph,
    id: &ParamId,
) -> Result<&'a crate::ParameterDef, IrError> {
    check_id("parameter", &id.0)?;
    graph
        .parameters
        .get(&id.0)
        .ok_or_else(|| IrError::DanglingParameter(id.clone()))
}

/// Validate nested references, collect dependencies, and report whether the
/// value contains a node connection, including repeated connections.
fn check_value<'a>(
    graph: &CanonicalGraph,
    value: &'a IrValue,
    ext_node_deps: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    dependencies: &mut BTreeSet<&'a str>,
) -> Result<bool, IrError> {
    let mut has_connection = false;
    match value {
        IrValue::Quantity(quantity) if !quantity.canonical_value().is_finite() => {
            return Err(IrError::NonFiniteNumber);
        }
        IrValue::Scalar(value) if !value.is_finite() => return Err(IrError::NonFiniteNumber),
        IrValue::Vec3(values) if !values.iter().all(|value| value.is_finite()) => {
            return Err(IrError::NonFiniteNumber);
        }
        IrValue::NodeRef { node_id, port } => {
            check_id("node", &node_id.0)?;
            if !graph.nodes.contains_key(&node_id.0) {
                return Err(IrError::DanglingNode(node_id.clone()));
            }
            if port != OUTPUT_PORT {
                return Err(IrError::UnknownOutputPort {
                    node_id: node_id.clone(),
                    port: port.clone(),
                });
            }
            dependencies.insert(&node_id.0);
            has_connection = true;
        }
        IrValue::BlobRef(handle) => {
            check_id("blob", &handle.0)?;
            if !graph.blobs.contains_key(&handle.0) {
                return Err(IrError::DanglingBlob(handle.clone()));
            }
        }
        IrValue::ExternalRef(ref_id) => {
            check_id("external_ref", &ref_id.0)?;
            let Some(transitive_nodes) = ext_node_deps.get(ref_id.0.as_str()) else {
                return Err(IrError::DanglingExternalRef(ref_id.clone()));
            };
            dependencies.extend(transitive_nodes.iter().copied());
        }
        IrValue::List(values) => {
            for value in values {
                has_connection |= check_value(graph, value, ext_node_deps, dependencies)?;
            }
        }
        IrValue::Map(values) => {
            for value in values.values() {
                has_connection |= check_value(graph, value, ext_node_deps, dependencies)?;
            }
        }
        _ => {}
    }
    Ok(has_connection)
}

fn value_type(graph: &CanonicalGraph, value: &IrValue) -> Option<IrType> {
    Some(match value {
        IrValue::Quantity(quantity) => quantity.ir_type(),
        IrValue::Scalar(_) => IrType::Scalar,
        IrValue::Int(_) => IrType::Int,
        IrValue::Bool(_) => IrType::Bool,
        IrValue::String(_) => IrType::String,
        IrValue::Vec3(_) => IrType::Vec3Dir,
        IrValue::NodeRef { node_id, .. } => graph.nodes.get(&node_id.0)?.output_type,
        IrValue::BlobRef(_) => IrType::BlobRef,
        IrValue::ExternalRef(_) => IrType::ExternalRef,
        IrValue::TopoRefExpr(_) => IrType::TopoRef,
        IrValue::List(values)
            if values.len() == 2
                && values.iter().all(
                    |value| matches!(value, IrValue::Quantity(q) if q.ir_type() == IrType::Length),
                ) =>
        {
            IrType::Vec2Length
        }
        IrValue::List(_) | IrValue::Map(_) => return None,
    })
}

fn check_value_type(
    location: String,
    value: &IrValue,
    expected: &[IrType],
    actual: Option<IrType>,
) -> Result<(), IrError> {
    if matches!(value, IrValue::Vec3(_)) && expected.contains(&IrType::Vec3Length) {
        return Ok(());
    }
    require_type(location, expected, actual)
}

fn check_input_type(
    graph: &CanonicalGraph,
    id: &str,
    port: &str,
    value: &IrValue,
    expected: ports::InputType,
) -> Result<(), IrError> {
    use ports::InputType::{List, Value, ValueOrList};

    let location = format!("node `{id}` input `{port}`");
    match (expected, value) {
        (List(types) | ValueOrList { item: types, .. }, IrValue::List(values)) => {
            for (index, value) in values.iter().enumerate() {
                check_value_type(
                    format!("{location} item {index}"),
                    value,
                    types,
                    value_type(graph, value),
                )?;
            }
            Ok(())
        }
        (Value(types) | ValueOrList { value: types, .. }, _) => {
            check_value_type(location, value, types, value_type(graph, value))
        }
        (List(types), _) => Err(IrError::TypeMismatch {
            location: format!("{location} (list of values)"),
            expected: types.to_vec(),
            actual: value_type(graph, value),
        }),
    }
}

fn check_output_type(id: &str, expected: &[IrType], actual: IrType) -> Result<(), IrError> {
    require_type(format!("node `{id}` output"), expected, Some(actual))
}

fn require_type(
    location: String,
    expected: &[IrType],
    actual: Option<IrType>,
) -> Result<(), IrError> {
    if actual.is_some_and(|actual| expected.contains(&actual)) {
        Ok(())
    } else {
        Err(IrError::TypeMismatch {
            location,
            expected: expected.to_vec(),
            actual,
        })
    }
}

fn execution_order<'a>(
    mut incoming: BTreeMap<&'a str, usize>,
    dependents: BTreeMap<&'a str, Vec<&'a str>>,
) -> Result<Vec<NodeId>, IrError> {
    let mut ready: BTreeSet<_> = incoming
        .iter()
        .filter_map(|(&id, &count)| (count == 0).then_some(id))
        .collect();
    let mut order = Vec::with_capacity(incoming.len());
    while let Some(id) = ready.pop_first() {
        order.push(NodeId::from(id));
        if let Some(nodes) = dependents.get(id) {
            for &dependent in nodes {
                let count = incoming
                    .get_mut(dependent)
                    .expect("each dependent has an in-degree");
                *count -= 1;
                if *count == 0 {
                    ready.insert(dependent);
                }
            }
        }
    }
    if order.len() == incoming.len() {
        Ok(order)
    } else {
        Err(IrError::CycleDetected)
    }
}

//! Read-only document summary, context slice, and entity explanation.

use std::collections::{BTreeMap, BTreeSet};

use cad_ir::{
    CanonicalGraph, ContextSliceOptions, DesignAssertionKind, FeatureSpec, NodeId, ParamId,
    compute_node_merkle_hashes, extract_context_slice, resolve_formula_values, validate_graph,
};

use crate::checks::require_param;
use crate::command::DesignContract;
use crate::dto::{
    ContextSliceRequest, ContextSliceResult, DocumentSummary, EntityExplanation, ExplainTarget,
    FeatureOutlineEntry, FormulaLink, ParameterSummary, ValidationSummary,
};
use crate::error::SemanticApiError;
use crate::walk::{
    CONTRACT_NODE_ID, STDLIB_META_KEY, feature_label, is_suppressed, leaf_outputs, node_topo_refs,
    node_upstream, read_outline,
};

/// Parameters, feature tree, components, terminal outputs, validation, and contract.
pub fn inspect_document_summary(graph: &CanonicalGraph) -> DocumentSummary {
    let resolved = resolve_formula_values(graph).ok();
    let parameters = graph
        .parameters
        .values()
        .map(|def| ParameterSummary {
            name: def.name.0.clone(),
            ir_type: def.ir_type,
            value: resolved
                .as_ref()
                .and_then(|(_, values)| values.get(&def.name).cloned())
                .unwrap_or_else(|| def.value.clone()),
            formula: def.formula.clone(),
        })
        .collect();

    let node_merkle: BTreeMap<String, String> = compute_node_merkle_hashes(graph)
        .unwrap_or_default()
        .into_iter()
        .map(|(node_id, hash)| (node_id.0, hash))
        .collect();

    let outline_ids = read_outline(graph);
    let mut feature_outline = Vec::new();
    let mut components = Vec::new();
    for id in &outline_ids {
        let Some(node) = graph.nodes.get(&id.0) else {
            continue;
        };
        let Ok(spec) = node.try_feature_spec() else {
            continue;
        };
        let entry = FeatureOutlineEntry {
            node_id: id.clone(),
            name: node.name.clone(),
            feature: feature_label(&spec).to_owned(),
            suppressed: is_suppressed(node),
            merkle_hash: node_merkle.get(&id.0).cloned().unwrap_or_default(),
        };
        if matches!(spec, FeatureSpec::ComponentInstance { .. }) {
            components.push(entry.clone());
        }
        feature_outline.push(entry);
    }

    let validation = match validate_graph(graph) {
        Ok(execution_order) => ValidationSummary {
            valid: true,
            execution_order,
            message: None,
        },
        Err(err) => ValidationSummary {
            valid: false,
            execution_order: Vec::new(),
            message: Some(err.to_string()),
        },
    };

    DocumentSummary {
        graph_id: graph.graph_id.clone(),
        units: graph.units,
        parameters,
        feature_outline,
        components,
        outputs: leaf_outputs(graph, &outline_ids),
        validation,
        design_contract: read_design_contract(graph),
        node_merkle,
    }
}

/// Wrap [`cad_ir::extract_context_slice`] and report which nodes and parameters were kept.
pub fn slice_document_context(
    graph: &CanonicalGraph,
    request: &ContextSliceRequest,
) -> Result<ContextSliceResult, SemanticApiError> {
    let sliced = extract_context_slice(
        graph,
        &ContextSliceOptions {
            focus_nodes: request.focus_nodes.clone(),
            upstream_depth: request.upstream_depth,
            downstream_depth: request.downstream_depth,
            ensure_valid_closure: request.ensure_valid_closure,
        },
    )
    .map_err(SemanticApiError::from_ir)?;
    let kept_nodes = sliced
        .nodes
        .keys()
        .map(|id| NodeId::from(id.as_str()))
        .collect();
    let kept_parameters = sliced.parameters.keys().cloned().collect();
    Ok(ContextSliceResult {
        graph: sliced,
        kept_nodes,
        kept_parameters,
    })
}

/// Explain a feature or a parameter: formula chain, creator, inputs, and topology refs.
pub fn explain_entity(
    graph: &CanonicalGraph,
    target: &ExplainTarget,
) -> Result<EntityExplanation, SemanticApiError> {
    match target {
        ExplainTarget::Node { node_id } => explain_node(graph, node_id),
        ExplainTarget::Parameter { name } => explain_parameter(graph, name),
    }
}

fn explain_node(
    graph: &CanonicalGraph,
    node_id: &NodeId,
) -> Result<EntityExplanation, SemanticApiError> {
    let node = graph
        .nodes
        .get(&node_id.0)
        .ok_or_else(|| SemanticApiError::not_found(format!("node `{node_id}` was not found")))?;
    let seeds = node
        .try_feature_spec()
        .map(|spec| spec.referenced_params().into_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(EntityExplanation {
        id: node.id.0.clone(),
        display_name: node.name.clone(),
        feature_node: node.try_feature_spec().is_ok().then(|| node.id.clone()),
        parameter_chain: parameter_chain(graph, &seeds)?,
        upstream_inputs: node_upstream(node),
        topo_refs: node_topo_refs(node),
        stdlib_resolution: node.metadata.get(STDLIB_META_KEY).cloned(),
    })
}

fn explain_parameter(
    graph: &CanonicalGraph,
    name: &str,
) -> Result<EntityExplanation, SemanticApiError> {
    let param_id = ParamId::from(name);
    require_param(graph, &param_id)?;
    let feature_node = read_outline(graph).into_iter().find(|id| {
        graph
            .nodes
            .get(&id.0)
            .and_then(|node| node.try_feature_spec().ok())
            .is_some_and(|spec| spec.referenced_params().contains(&param_id))
    });
    Ok(EntityExplanation {
        id: name.to_owned(),
        display_name: name.to_owned(),
        feature_node,
        parameter_chain: parameter_chain(graph, &[param_id])?,
        upstream_inputs: Vec::new(),
        topo_refs: Vec::new(),
        stdlib_resolution: None,
    })
}

fn parameter_chain(
    graph: &CanonicalGraph,
    seeds: &[ParamId],
) -> Result<Vec<FormulaLink>, SemanticApiError> {
    let mut needed: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<ParamId> = seeds.to_vec();
    let mut depends_on: BTreeMap<String, Vec<String>> = BTreeMap::new();

    while let Some(param_id) = stack.pop() {
        if !needed.insert(param_id.0.clone()) {
            continue;
        }
        let def = require_param(graph, &param_id)?;
        let deps = if let Some(formula) = &def.formula {
            cad_ir::collect_formula_dependencies(&def.name, formula)
                .map_err(SemanticApiError::from_ir)?
        } else {
            BTreeSet::new()
        };
        let mut names: Vec<String> = deps.iter().map(|dep| dep.0.clone()).collect();
        names.sort();
        for dep in deps {
            stack.push(dep);
        }
        depends_on.insert(param_id.0, names);
    }

    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let mut ordered = Vec::new();
    while emitted.len() < needed.len() {
        let ready: Vec<String> = needed
            .iter()
            .filter(|name| {
                !emitted.contains(*name)
                    && depends_on[*name]
                        .iter()
                        .all(|dep| emitted.contains(dep) || !needed.contains(dep))
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            return Err(SemanticApiError::validation(
                "parameter formula chain contains a cycle",
            ));
        }
        for name in ready {
            let def = &graph.parameters[&name];
            emitted.insert(name.clone());
            ordered.push(FormulaLink {
                name: name.clone(),
                ir_type: def.ir_type,
                formula: def.formula.clone(),
                value: def.value.clone(),
                depends_on: depends_on.remove(&name).unwrap_or_default(),
            });
        }
    }
    Ok(ordered)
}

fn read_design_contract(graph: &CanonicalGraph) -> Option<DesignContract> {
    let node = graph.nodes.get(CONTRACT_NODE_ID)?;
    let FeatureSpec::DesignAssertion {
        kind: DesignAssertionKind::Custom { expression, .. },
        ..
    } = node.try_feature_spec().ok()?
    else {
        return None;
    };
    serde_json::from_str(&expression).ok()
}

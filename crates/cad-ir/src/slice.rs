//! Local subgraph context slicing for low-token AI-assisted large assembly modeling.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    CanonicalGraph, FEATURE_SPEC_PORT, IrError, IrValue, NodeId, NodeKind,
    formula::collect_formula_dependencies, validate_graph,
};

const fn default_ensure_valid_closure() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSliceOptions {
    #[serde(default)]
    pub focus_nodes: Vec<NodeId>,
    #[serde(default)]
    pub upstream_depth: usize,
    #[serde(default)]
    pub downstream_depth: usize,
    /// When true (default), automatically include transitive upstream ancestors required
    /// so every kept node has valid `NodeRef` inputs and the sliced graph passes `validate_graph`.
    #[serde(default = "default_ensure_valid_closure")]
    pub ensure_valid_closure: bool,
}

impl Default for ContextSliceOptions {
    fn default() -> Self {
        Self {
            focus_nodes: Vec::new(),
            upstream_depth: 0,
            downstream_depth: 0,
            ensure_valid_closure: true,
        }
    }
}

/// Extract a local subgraph around `options.focus_nodes` bounded by upstream/downstream step
/// depths, optionally closed over all transitive upstream `NodeRef` dependencies so the resulting
/// `CanonicalGraph` is self-contained and valid for `.kcl` projection.
pub fn extract_context_slice(
    graph: &CanonicalGraph,
    options: &ContextSliceOptions,
) -> Result<CanonicalGraph, IrError> {
    validate_graph(graph)?;

    for focus in &options.focus_nodes {
        if !graph.nodes.contains_key(&focus.0) {
            return Err(IrError::DanglingNode(focus.clone()));
        }
    }

    let mut upstream_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut downstream_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (id, node) in &graph.nodes {
        let mut deps = BTreeSet::new();
        let mut visited_ext = BTreeSet::new();
        for value in node.inputs.values() {
            collect_node_deps(graph, value, &mut visited_ext, &mut deps);
        }
        for &dep in &deps {
            downstream_of.entry(dep).or_default().insert(id.as_str());
        }
        upstream_of.insert(id.as_str(), deps);
    }

    let upstream_candidates = bfs_steps(&options.focus_nodes, options.upstream_depth, &upstream_of);
    let downstream_candidates = bfs_steps(
        &options.focus_nodes,
        options.downstream_depth,
        &downstream_of,
    );

    let mut kept_nodes: BTreeSet<&str> = upstream_candidates
        .union(&downstream_candidates)
        .copied()
        .collect();

    if options.ensure_valid_closure {
        let mut queue: VecDeque<&str> = kept_nodes.iter().copied().collect();
        while let Some(curr) = queue.pop_front() {
            if let Some(parents) = upstream_of.get(curr) {
                for &parent in parents {
                    if kept_nodes.insert(parent) {
                        queue.push_back(parent);
                    }
                }
            }
        }
    }

    let mut kept_params: BTreeSet<String> = BTreeSet::new();
    let mut kept_blobs: BTreeSet<&str> = BTreeSet::new();
    let mut kept_ext_refs: BTreeSet<&str> = BTreeSet::new();

    for &node_id in &kept_nodes {
        let node = &graph.nodes[node_id];
        if let NodeKind::ParameterRef { param_id } = &node.kind {
            kept_params.insert(param_id.0.clone());
            if let Some(parameter) = graph.parameters.get(&param_id.0) {
                collect_resource_refs(graph, &parameter.value, &mut kept_blobs, &mut kept_ext_refs);
            }
        }
        if node.inputs.contains_key(FEATURE_SPEC_PORT)
            && let Ok(spec) = node.try_feature_spec()
        {
            for param_id in spec.referenced_params() {
                kept_params.insert(param_id.0);
            }
        }
        for value in node.inputs.values() {
            collect_resource_refs(graph, value, &mut kept_blobs, &mut kept_ext_refs);
        }
    }

    // Transitively include upstream parameters referenced by kept parameter formulas.
    let mut param_queue: VecDeque<String> = kept_params.iter().cloned().collect();
    while let Some(param_name) = param_queue.pop_front() {
        if let Some(def) = graph.parameters.get(&param_name)
            && let Some(raw_formula) = def.formula.as_deref()
            && let Ok(deps) = collect_formula_dependencies(&def.name, raw_formula)
        {
            for dep in deps {
                if kept_params.insert(dep.0.clone()) {
                    if let Some(dep_def) = graph.parameters.get(&dep.0) {
                        collect_resource_refs(
                            graph,
                            &dep_def.value,
                            &mut kept_blobs,
                            &mut kept_ext_refs,
                        );
                    }
                    param_queue.push_back(dep.0);
                }
            }
        }
    }

    let parameters = kept_params
        .into_iter()
        .filter_map(|id| {
            graph
                .parameters
                .get(&id)
                .map(|def| (id, def.clone()))
        })
        .collect();

    let blobs = kept_blobs
        .into_iter()
        .filter_map(|handle| {
            graph
                .blobs
                .get(handle)
                .map(|entry| (handle.to_owned(), entry.clone()))
        })
        .collect();

    let external_refs = kept_ext_refs
        .into_iter()
        .filter_map(|ref_id| {
            graph
                .external_refs
                .get(ref_id)
                .map(|def| (ref_id.to_owned(), def.clone()))
        })
        .collect();

    let nodes = kept_nodes
        .iter()
        .filter_map(|&id| {
            graph
                .nodes
                .get(id)
                .map(|node| (id.to_owned(), node.clone()))
        })
        .collect();

    let mut outputs: Vec<NodeId> = graph
        .outputs
        .iter()
        .filter(|id| kept_nodes.contains(id.0.as_str()))
        .cloned()
        .collect();

    if outputs.is_empty() {
        for &id in &kept_nodes {
            let has_kept_consumer = downstream_of.get(id).is_some_and(|consumers| {
                consumers
                    .iter()
                    .any(|&consumer| consumer != id && kept_nodes.contains(consumer))
            });
            if !has_kept_consumer {
                outputs.push(NodeId::from(id));
            }
        }
    }

    let sliced = CanonicalGraph {
        ir_schema_version: graph.ir_schema_version,
        graph_id: graph.graph_id.clone(),
        units: graph.units,
        parameters,
        blobs,
        external_refs,
        nodes,
        outputs,
    };

    if options.ensure_valid_closure {
        validate_graph(&sliced)?;
    }

    Ok(sliced)
}

fn bfs_steps<'a>(
    seeds: &'a [NodeId],
    max_depth: usize,
    adjacency: &BTreeMap<&'a str, BTreeSet<&'a str>>,
) -> BTreeSet<&'a str> {
    let mut visited: BTreeMap<&'a str, usize> = BTreeMap::new();
    let mut queue: VecDeque<(&'a str, usize)> = VecDeque::new();

    for seed in seeds {
        let id = seed.0.as_str();
        if visited.insert(id, 0).is_none() {
            queue.push_back((id, 0));
        }
    }

    while let Some((curr, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = adjacency.get(curr) {
            for &next in neighbors {
                let next_depth = depth + 1;
                if visited.get(next).is_none_or(|&prev| next_depth < prev) {
                    visited.insert(next, next_depth);
                    queue.push_back((next, next_depth));
                }
            }
        }
    }

    visited.into_keys().collect()
}

fn collect_node_deps<'a>(
    graph: &'a CanonicalGraph,
    value: &'a IrValue,
    visited_ext: &mut BTreeSet<&'a str>,
    deps: &mut BTreeSet<&'a str>,
) {
    match value {
        IrValue::NodeRef { node_id, .. } => {
            deps.insert(node_id.0.as_str());
        }
        IrValue::ExternalRef(ref_id) => {
            if visited_ext.insert(ref_id.0.as_str())
                && let Some(ext_def) = graph.external_refs.get(&ref_id.0)
            {
                for override_val in ext_def.overrides.values() {
                    collect_node_deps(graph, override_val, visited_ext, deps);
                }
            }
        }
        IrValue::List(items) => {
            for item in items {
                collect_node_deps(graph, item, visited_ext, deps);
            }
        }
        IrValue::Map(entries) => {
            for item in entries.values() {
                collect_node_deps(graph, item, visited_ext, deps);
            }
        }
        _ => {}
    }
}

fn collect_resource_refs<'a>(
    graph: &'a CanonicalGraph,
    value: &'a IrValue,
    blobs: &mut BTreeSet<&'a str>,
    ext_refs: &mut BTreeSet<&'a str>,
) {
    match value {
        IrValue::BlobRef(handle) => {
            blobs.insert(handle.0.as_str());
        }
        IrValue::ExternalRef(ref_id) => {
            if ext_refs.insert(ref_id.0.as_str())
                && let Some(ext_def) = graph.external_refs.get(&ref_id.0)
            {
                for override_val in ext_def.overrides.values() {
                    collect_resource_refs(graph, override_val, blobs, ext_refs);
                }
            }
        }
        IrValue::List(items) => {
            for item in items {
                collect_resource_refs(graph, item, blobs, ext_refs);
            }
        }
        IrValue::Map(entries) => {
            for item in entries.values() {
                collect_resource_refs(graph, item, blobs, ext_refs);
            }
        }
        _ => {}
    }
}

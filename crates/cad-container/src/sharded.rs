//! Sharded Git-friendly graph directory serialization (`graphs/00_header.ir.json` + `graphs/<shard>.ir.json`).

use std::collections::{BTreeMap, btree_map::Entry};

use cad_ir::{
    BlobManifestEntry, CanonicalGraph, ExternalRefDef, IrError, IrNode, NodeId, ParameterDef,
    UnitSystem, validate_graph,
};
use serde::{Deserialize, Serialize};

use crate::{
    ContainerError, blobs::normalize_relative_path, manifest, manifest::SHARDED_ENTRY_GRAPH_PATH,
};

/// Default module shard name for nodes without `metadata["module"]`.
pub const DEFAULT_GRAPH_SHARD: &str = "default";

/// Header document persisted at `graphs/00_header.ir.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardedGraphHeader {
    pub ir_schema_version: u32,
    pub graph_id: String,
    pub units: UnitSystem,
    pub parameters: BTreeMap<String, ParameterDef>,
    pub blobs: BTreeMap<String, BlobManifestEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub external_refs: BTreeMap<String, ExternalRefDef>,
    pub outputs: Vec<NodeId>,
}

/// Per-module node shard persisted at `graphs/<shard>.ir.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNodeShard {
    pub shard: String,
    pub nodes: BTreeMap<String, IrNode>,
}

#[derive(Deserialize)]
struct HeaderVersionPreflight {
    #[serde(default)]
    ir_schema_version: Option<u64>,
}

/// Split a validated `CanonicalGraph` into `graphs/00_header.ir.json` and per-module `graphs/<shard>.ir.json` byte entries.
pub fn split_graph_into_shards(
    graph: &CanonicalGraph,
) -> Result<BTreeMap<String, Vec<u8>>, ContainerError> {
    validate_graph(graph)?;

    let header = ShardedGraphHeader {
        ir_schema_version: graph.ir_schema_version,
        graph_id: graph.graph_id.clone(),
        units: graph.units,
        parameters: graph.parameters.clone(),
        blobs: graph.blobs.clone(),
        external_refs: graph.external_refs.clone(),
        outputs: graph.outputs.clone(),
    };

    let mut buckets: BTreeMap<String, BTreeMap<String, IrNode>> = BTreeMap::new();
    for (node_id, node) in &graph.nodes {
        let raw_module = node
            .metadata
            .get("module")
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_GRAPH_SHARD);
        let shard_name = checked_shard_name(raw_module)?;
        buckets
            .entry(shard_name)
            .or_default()
            .insert(node_id.clone(), node.clone());
    }

    let mut files = BTreeMap::new();
    files.insert(
        SHARDED_ENTRY_GRAPH_PATH.to_owned(),
        serde_json::to_vec_pretty(&header)?,
    );

    for (shard_name, nodes) in buckets {
        let path = format!("graphs/{shard_name}.ir.json");
        let shard_doc = GraphNodeShard {
            shard: shard_name,
            nodes,
        };
        files.insert(path, serde_json::to_vec_pretty(&shard_doc)?);
    }

    Ok(files)
}

/// Merge `graphs/00_header.ir.json` and sorted `graphs/*.ir.json` shards into a validated `CanonicalGraph`.
pub fn merge_sharded_graph(
    header_json: &str,
    shard_jsons_in_lexical_order: &[(String, String)],
) -> Result<CanonicalGraph, ContainerError> {
    let preflight: HeaderVersionPreflight = serde_json::from_str(header_json)?;
    if let Some(found) = preflight.ir_schema_version {
        manifest::validate_graph_schema_version(found)?;
    }
    let header: ShardedGraphHeader = serde_json::from_str(header_json)?;
    manifest::validate_graph_schema_version(header.ir_schema_version.into())?;

    let mut merged_nodes = BTreeMap::new();
    for (shard_path, shard_json) in shard_jsons_in_lexical_order {
        if shard_path.eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH) {
            continue;
        }
        let shard_doc: GraphNodeShard = serde_json::from_str(shard_json)?;
        for (key, node) in shard_doc.nodes {
            if key != node.id.0 {
                return Err(ContainerError::Ir(IrError::IdMismatch {
                    entity: "node",
                    key,
                    id: node.id.0,
                }));
            }
            match merged_nodes.entry(key) {
                Entry::Vacant(slot) => {
                    slot.insert(node);
                }
                Entry::Occupied(existing) => {
                    return Err(ContainerError::Ir(IrError::DuplicateId {
                        entity: "node",
                        id: existing.key().clone(),
                    }));
                }
            }
        }
    }

    let mut graph = CanonicalGraph {
        ir_schema_version: header.ir_schema_version,
        graph_id: header.graph_id,
        units: header.units,
        parameters: header.parameters,
        blobs: header.blobs,
        external_refs: header.external_refs,
        nodes: merged_nodes,
        outputs: header.outputs,
    };
    validate_graph(&graph)?;
    graph.evaluate_formulas()?;
    Ok(graph)
}

pub(crate) fn checked_shard_name(raw: &str) -> Result<String, ContainerError> {
    let normalized = normalize_relative_path(raw)?;
    if normalized.contains('/') || normalized.eq_ignore_ascii_case("00_header") {
        return Err(ContainerError::UnsafePath(raw.to_owned()));
    }
    Ok(normalized)
}

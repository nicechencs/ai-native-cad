//! Node-level deterministic Merkle hashing for geometry cache keys and branch reuse.

use std::{collections::BTreeMap, fmt::Write};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    BlobKind, CanonicalGraph, ExternalRefDef, ExternalRefId, FEATURE_SPEC_PORT, IrError, IrType,
    IrValue, NodeId, NodeKind, PayloadPolicy, Quantity, UnitSystem, validate_graph, wire,
};

const NODE_MERKLE_DOMAIN: &str = "cad-ir:node-merkle:v1";

/// Compute deterministic 64-character lowercase hex SHA-256 Merkle hashes for every node
/// in topological execution order.
pub fn compute_node_merkle_hashes(
    graph: &CanonicalGraph,
) -> Result<BTreeMap<NodeId, String>, IrError> {
    let order = validate_graph(graph)?;
    let mut hashes = BTreeMap::new();

    for node_id in order {
        let node = &graph.nodes[&node_id.0];
        let parameter = match &node.kind {
            NodeKind::ParameterRef { param_id } => {
                let def = &graph.parameters[&param_id.0];
                Some(ParameterMerklePayload {
                    ir_type: def.ir_type,
                    value: MerkleValue::from_ir_value(graph, &def.value, &hashes),
                    formula: def.formula.as_deref(),
                })
            }
            _ => None,
        };

        let mut bound_parameters = BTreeMap::new();
        if node.inputs.contains_key(FEATURE_SPEC_PORT) {
            let spec = node.try_feature_spec()?;
            for param_id in spec.referenced_params() {
                if let Some(def) = graph.parameters.get(&param_id.0) {
                    bound_parameters.insert(
                        param_id.0,
                        ParameterMerklePayload {
                            ir_type: def.ir_type,
                            value: MerkleValue::from_ir_value(graph, &def.value, &hashes),
                            formula: def.formula.as_deref(),
                        },
                    );
                }
            }
        }

        let inputs: BTreeMap<&str, MerkleValue<'_>> = node
            .inputs
            .iter()
            .map(|(port, value)| {
                (
                    port.as_str(),
                    MerkleValue::from_ir_value(graph, value, &hashes),
                )
            })
            .collect();

        let payload = NodeMerklePayload {
            domain: NODE_MERKLE_DOMAIN,
            units: &graph.units,
            kind: &node.kind,
            output_type: node.output_type,
            kernel_op_version: &node.kernel_op_version,
            parameter,
            bound_parameters,
            inputs,
        };

        let bytes = serde_json::to_vec(&payload)?;
        let digest = Sha256::digest(&bytes);
        let mut hex = String::with_capacity(64);
        for byte in digest {
            let _ = write!(&mut hex, "{byte:02x}");
        }
        hashes.insert(node_id, hex);
    }

    Ok(hashes)
}

#[derive(Serialize)]
struct NodeMerklePayload<'a> {
    domain: &'static str,
    units: &'a UnitSystem,
    kind: &'a NodeKind,
    output_type: IrType,
    kernel_op_version: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameter: Option<ParameterMerklePayload<'a>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    bound_parameters: BTreeMap<String, ParameterMerklePayload<'a>>,
    inputs: BTreeMap<&'a str, MerkleValue<'a>>,
}

#[derive(Serialize)]
struct ParameterMerklePayload<'a> {
    ir_type: IrType,
    value: MerkleValue<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    formula: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum MerkleValue<'a> {
    Quantity(&'a Quantity),
    Scalar(#[serde(serialize_with = "wire::number")] f64),
    Int(i64),
    Bool(bool),
    String(&'a str),
    Vec3(#[serde(serialize_with = "wire::vec3")] [f64; 3]),
    NodeRef {
        upstream_merkle: &'a str,
        port: &'a str,
    },
    BlobRef {
        kind: BlobKind,
        sha256: &'a str,
    },
    ExternalRef(ExternalRefMerklePayload<'a>),
    TopoRefExpr(&'a str),
    List(Vec<MerkleValue<'a>>),
    Map(BTreeMap<&'a str, MerkleValue<'a>>),
}

#[derive(Serialize)]
struct ExternalRefMerklePayload<'a> {
    ref_id: &'a ExternalRefId,
    container_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_sha256: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_graph_id: &'a Option<String>,
    payload_policy: PayloadPolicy,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    overrides: BTreeMap<&'a str, MerkleValue<'a>>,
}

impl<'a> MerkleValue<'a> {
    fn from_ir_value(
        graph: &'a CanonicalGraph,
        value: &'a IrValue,
        hashes: &'a BTreeMap<NodeId, String>,
    ) -> Self {
        match value {
            IrValue::Quantity(quantity) => Self::Quantity(quantity),
            IrValue::Scalar(number) => Self::Scalar(*number),
            IrValue::Int(number) => Self::Int(*number),
            IrValue::Bool(flag) => Self::Bool(*flag),
            IrValue::String(text) => Self::String(text.as_str()),
            IrValue::Vec3(coords) => Self::Vec3(*coords),
            IrValue::NodeRef { node_id, port } => Self::NodeRef {
                upstream_merkle: hashes
                    .get(node_id)
                    .expect("validated topological order computes upstream hashes first")
                    .as_str(),
                port: port.as_str(),
            },
            IrValue::BlobRef(handle) => {
                let entry = &graph.blobs[&handle.0];
                Self::BlobRef {
                    kind: entry.kind,
                    sha256: entry.sha256.as_str(),
                }
            }
            IrValue::ExternalRef(ref_id) => {
                let def = &graph.external_refs[&ref_id.0];
                Self::ExternalRef(ExternalRefMerklePayload::from_def(graph, def, hashes))
            }
            IrValue::TopoRefExpr(expr) => Self::TopoRefExpr(expr.as_str()),
            IrValue::List(items) => Self::List(
                items
                    .iter()
                    .map(|item| Self::from_ir_value(graph, item, hashes))
                    .collect(),
            ),
            IrValue::Map(entries) => Self::Map(
                entries
                    .iter()
                    .map(|(key, item)| (key.as_str(), Self::from_ir_value(graph, item, hashes)))
                    .collect(),
            ),
        }
    }
}

impl<'a> ExternalRefMerklePayload<'a> {
    fn from_def(
        graph: &'a CanonicalGraph,
        def: &'a ExternalRefDef,
        hashes: &'a BTreeMap<NodeId, String>,
    ) -> Self {
        let overrides = def
            .overrides
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str(),
                    MerkleValue::from_ir_value(graph, value, hashes),
                )
            })
            .collect();
        Self {
            ref_id: &def.ref_id,
            container_path: def.container_path.as_str(),
            expected_sha256: &def.expected_sha256,
            target_graph_id: &def.target_graph_id,
            payload_policy: def.payload_policy,
            overrides,
        }
    }
}

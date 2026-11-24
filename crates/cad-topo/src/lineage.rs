//! Feature-evolution references for persistent topology naming.

use serde::{Deserialize, Serialize};

use crate::fingerprint::GeometricFingerprint;

/// Ephemeral identifier assigned by a geometry kernel during one evaluation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TopoEntityId(pub String);

impl From<String> for TopoEntityId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TopoEntityId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// A topology operation recorded while a feature output evolves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvolutionOp {
    Created,
    Survived,
    Split { part_index: u32 },
    Merged { sources: Vec<String> },
    Trimmed,
    Deleted,
}

/// One operation in a lineage chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvolutionRecord {
    pub step_node_id: String,
    pub op: EvolutionOp,
    pub note: Option<String>,
}

/// Durable, kernel-neutral reference to a feature output topology entity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageTopoRef {
    pub ref_id: String,
    pub creator_node_id: String,
    pub local_output_slot: String,
    pub evolution_chain: Vec<EvolutionRecord>,
    pub fingerprint: GeometricFingerprint,
    pub provenance_tags: Vec<String>,
}

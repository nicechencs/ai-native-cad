use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{IrError, IrType, Quantity, UnitSystem, validate_graph, wire};

macro_rules! identifier {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

identifier!(/// A stable node identifier within a graph.
    NodeId);
identifier!(/// A parameter name in the graph's parameter table.
    ParamId);
identifier!(/// A logical handle to a payload stored outside the graph.
    BlobHandle);
identifier!(/// A logical reference identifier to an external container or sub-assembly.
    ExternalRefId);

/// The v1 graph exposes a single typed output on each node.
pub const OUTPUT_PORT: &str = "out";

/// An opaque topology selector interpreted by the topology subsystem.
pub type TopoRefExpr = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IrValue {
    Quantity(Quantity),
    Scalar(#[serde(serialize_with = "wire::number")] f64),
    Int(i64),
    Bool(bool),
    String(String),
    /// Coordinates or direction components, as determined by the receiving port.
    Vec3(#[serde(serialize_with = "wire::vec3")] [f64; 3]),
    NodeRef {
        node_id: NodeId,
        port: String,
    },
    BlobRef(BlobHandle),
    ExternalRef(ExternalRefId),
    TopoRefExpr(TopoRefExpr),
    List(Vec<IrValue>),
    Map(#[serde(deserialize_with = "wire::ordered_map")] BTreeMap<String, IrValue>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterDef {
    pub name: ParamId,
    pub ir_type: IrType,
    /// The typed literal value used by the graph.
    pub value: IrValue,
    /// Optional source formula for consumers that evaluate expressions.
    pub formula: Option<String>,
}

/// Machine-readable diagnostic attached to a failed node evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeErrorDetail {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_status: Option<i32>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeState {
    #[default]
    Valid,
    FailedWithRollback {
        reason: String,
        failed_topo_refs: Vec<String>,
    },
    Suppressed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TpmsFamily {
    Gyroid,
    SchwarzP,
    Diamond,
    Lidinoid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind {
    ParameterRef {
        param_id: ParamId,
    },
    #[serde(rename = "sketch_2d")]
    Sketch2D,
    PrimitiveBox,
    PrimitiveCylinder,
    Extrude,
    Revolve,
    Sweep,
    LoftGuided,
    Shell,
    Fillet,
    Chamfer,
    Draft,
    Mirror,
    PatternLinear,
    PatternPolar,
    BooleanUnion,
    BooleanSubtract,
    BooleanIntersect,
    Hole,
    DirectFaceEdit,
    SurfaceBoundary,
    SurfaceTrim,
    SurfaceKnitToSolid,
    #[serde(rename = "subd_from_blob")]
    SubDFromBlob,
    #[serde(rename = "subd_to_nurbs")]
    SubDToNurbs,
    FieldSdf,
    FieldTpms {
        family: TpmsFamily,
    },
    FieldRamp,
    FieldFromFeaBlob,
    ImplicitSmoothBlend,
    ImplicitLatticeInfill,
    StdLibCall,
    ImportAsset,
    AssemblyComponent,
    AssemblyMate,
    TagFaces,
    AssertGate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrNode {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    #[serde(deserialize_with = "wire::ordered_map")]
    pub inputs: BTreeMap<String, IrValue>,
    pub output_type: IrType,
    pub state: NodeState,
    pub kernel_op_version: Option<String>,
    #[serde(deserialize_with = "wire::ordered_map")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobKind {
    Spline,
    #[serde(rename = "subd")]
    SubD,
    Field,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobManifestEntry {
    pub handle: BlobHandle,
    pub kind: BlobKind,
    pub sha256: String,
    pub byte_len: u64,
    pub relative_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadPolicy {
    #[default]
    Eager,
    DeferredPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRefDef {
    pub ref_id: ExternalRefId,
    pub container_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_graph_id: Option<String>,
    #[serde(default)]
    pub payload_policy: PayloadPolicy,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "wire::ordered_map"
    )]
    pub overrides: BTreeMap<String, IrValue>,
}

pub const IR_SCHEMA_VERSION: u32 = 1;

const fn default_schema_version() -> u32 {
    IR_SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalGraph {
    #[serde(default = "default_schema_version")]
    pub ir_schema_version: u32,
    pub graph_id: String,
    pub units: UnitSystem,
    #[serde(deserialize_with = "wire::ordered_map")]
    pub parameters: BTreeMap<String, ParameterDef>,
    #[serde(deserialize_with = "wire::ordered_map")]
    pub blobs: BTreeMap<String, BlobManifestEntry>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "wire::ordered_map"
    )]
    pub external_refs: BTreeMap<String, ExternalRefDef>,
    #[serde(deserialize_with = "wire::ordered_map")]
    pub nodes: BTreeMap<String, IrNode>,
    pub outputs: Vec<NodeId>,
}

impl CanonicalGraph {
    /// Create an empty v1 graph with the base unit system.
    pub fn new(graph_id: impl Into<String>) -> Self {
        Self {
            ir_schema_version: IR_SCHEMA_VERSION,
            graph_id: graph_id.into(),
            units: UnitSystem::default(),
            parameters: BTreeMap::new(),
            blobs: BTreeMap::new(),
            external_refs: BTreeMap::new(),
            nodes: BTreeMap::new(),
            outputs: Vec::new(),
        }
    }

    /// Serialize compact JSON with ordered map keys and one encoding of zero.
    pub fn to_canonical_json(&self) -> Result<String, IrError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Deserialize a graph, validate its references/formulas/types/execution order,
    /// and refresh derived formula parameter values.
    pub fn from_json(s: &str) -> Result<Self, IrError> {
        let mut graph: Self = serde_json::from_str(s)?;
        validate_graph(&graph)?;
        graph.evaluate_formulas()?;
        Ok(graph)
    }
}

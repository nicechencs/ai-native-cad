//! Deterministic semantic topology queries.

use serde::{Deserialize, Serialize};

use crate::fingerprint::{CurveClass, EntityKind, SurfaceClass};
use crate::lineage::LineageTopoRef;

/// A principal direction used by a normal filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AxisDir {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
    Custom([f64; 3]),
}

impl AxisDir {
    pub(crate) fn vector(&self) -> [f64; 3] {
        match self {
            Self::PosX => [1.0, 0.0, 0.0],
            Self::NegX => [-1.0, 0.0, 0.0],
            Self::PosY => [0.0, 1.0, 0.0],
            Self::NegY => [0.0, -1.0, 0.0],
            Self::PosZ => [0.0, 0.0, 1.0],
            Self::NegZ => [0.0, 0.0, -1.0],
            Self::Custom(value) => *value,
        }
    }
}

/// A coordinate component used by a coordinate tie-breaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordAxis {
    X,
    Y,
    Z,
}

/// A predicate applied to candidate geometry and provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TopoFilter {
    Surface(SurfaceClass),
    Curve(CurveClass),
    Normal { dir: AxisDir, angle_tol_deg: f64 },
    Tagged(String),
    WithinBBox { min_mm: [f64; 3], max_mm: [f64; 3] },
}

/// An ordered deterministic way to rank otherwise matching candidates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TieBreaker {
    MaxByCoord(CoordAxis),
    MinByCoord(CoordAxis),
    MaxByRadius,
    MinByRadius,
    MaxByAreaOrLength,
    MinByAreaOrLength,
    NearestTo([f64; 3]),
}

/// A semantic reference resolved against a single topology snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryTopoRef {
    pub entity_kind: EntityKind,
    pub filters: Vec<TopoFilter>,
    pub tie_breakers: Vec<TieBreaker>,
    pub require_unique: bool,
}

/// The durable topology reference format supports both tracks.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TopoRef {
    Lineage(LineageTopoRef),
    Query(QueryTopoRef),
}

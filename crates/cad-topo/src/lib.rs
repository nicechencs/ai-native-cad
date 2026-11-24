//! Kernel-independent persistent references to entities in a topology snapshot.
//!
//! Lineage references follow feature outputs across evaluations. Query references
//! select entities by geometric and semantic criteria in the current snapshot.
//! Spatial queries, measurements, and recovery hints let an agent find a face or
//! edge from geometry instead of guessing a kernel slot name.

#![forbid(unsafe_code)]

pub mod evolution;
pub mod fingerprint;
pub mod hints;
pub mod lineage;
pub mod measure;
pub mod query;
pub mod resolver;
pub mod spatial_query;

pub use evolution::{
    EvolutionOptions, EvolutionTolerances, FreshSlotStyle, LineageParent, TopologySample,
    propagate_lineage,
};
pub use fingerprint::{
    CurveClass, EntityKind, GeometricFingerprint, MatchTolerances, SurfaceClass,
    fingerprint_similarity,
};
pub use hints::{RecoveryHint, TopoRecoveryCandidate, build_ambiguous_topo_hints};
pub use lineage::{EvolutionOp, EvolutionRecord, LineageTopoRef, TopoEntityId};
pub use measure::{
    MeasurementError, MeasurementKind, MeasurementOptions, MeasurementResult, measure_geometry,
    measure_geometry_refs,
};
pub use query::{AxisDir, CoordAxis, QueryTopoRef, TieBreaker, TopoFilter, TopoRef};
pub use resolver::{CandidateEntity, ResolveResult, TopoError, TopoSnapshot, resolve_topo_ref};
pub use spatial_query::{
    DEFAULT_NORMAL_ANGLE_TOL_DEG, DEFAULT_RADIUS_TOL_MM, TopologyHit, TopologyQueryFilter,
    TopologyQueryResult, TopologySortKey, query_topology,
};

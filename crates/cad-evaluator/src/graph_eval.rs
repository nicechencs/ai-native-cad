//! Incremental evaluation of Canonical Graph solids, implicit lattices, and SubD meshes.
//!
//! Bare coordinates use millimetres. Standard-library hole origins mark the
//! entry surface, with the drill extending toward negative Z. Geometry
//! assertions connect a `solid` and can supply enclosing `bbox_min` and
//! `bbox_max` coordinates alongside the optional boolean `condition`.

use std::{
    borrow::Cow,
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    path::{Path, PathBuf},
    time::Instant,
};
#[cfg(feature = "implicit")]
use std::sync::Arc;

use cad_assembly_solver::{
    AssemblyComponent, AssemblyMate, AssemblyMateKind, AssemblySystem, MateAnchor, RigidPose,
    SolveConfig, SolveReport,
};
use cad_container::{
    AicadProject, ContainerError, KernelFingerprint, LazyProjectReader, SqliteContainer,
    StorageKind, detect_storage_kind,
};
use cad_hir::{Diagnostic, Severity};
#[cfg(feature = "implicit")]
use cad_implicit::{
    BoxSdf, ConstantField, CylinderSdf, FieldRamp, ImplicitMesh, LatticeInfillSdf, ScalarField,
    StressDrivenThicknessField, TpmsField, TpmsSheetField, decode_voxel_field_blob,
};
use cad_ir::{
    BlobKind, BlobManifestEntry, CanonicalGraph, IrError, IrNode, IrType, IrValue, NodeErrorDetail,
    NodeId, NodeKind, NodeState, PayloadPolicy, compute_node_merkle_hashes,
};
#[cfg(feature = "implicit")]
use cad_ir::TpmsFamily;
use cad_kernel_occt::{
    BBox3, BRepEdge, BRepFace, CheckReport, ComponentMeta, CsgNode, CurveType, GeometryKernel,
    InterferenceViolation, KernelError, KernelOperationError, KernelPayload, PmiRecord, SolidShape,
    SurfaceType,
};
#[cfg(feature = "subd")]
use cad_kernel_occt::{QuadGridSurfaceSpec, quad_grid_to_bspline_solid};
use cad_mesh_io::LengthUnit;
use cad_scene_mesh::build_scene_mesh;
#[cfg(feature = "subd")]
use cad_subd::{SubDMesh, decode_subd_blob, subdivide, to_triangle_mesh};
use cad_syntax::Span;
use cad_topo::{
    CurveClass, EntityKind, EvolutionOptions, EvolutionTolerances, FreshSlotStyle,
    GeometricFingerprint, LineageParent, RecoveryHint, SurfaceClass, TopoRef, TopoSnapshot,
    TopologySample, propagate_lineage,
};
use sha2::{Digest, Sha256};

use crate::{
    EvaluationOutput,
    asset_resolver::{AssetResolveError, AssetResolver, NoopAssetResolver},
    eval_diagnostics::{DFM_MAX_BLOCKED_AREA_RATIO, DfmAccessibility, dfm_tool_accessibility},
    extref_eval::{
        DeferredComponentSummary, ExternalRefContext, ResolvedExtRefPayload,
        evaluate_external_ref_component, transform_bbox,
    },
    handles::hole_offset_handles,
    import_geometry::{
        ImportFormat, ImportOptions, evaluate_import_for_family, evaluate_import_with_kernel,
        operation_diagnostic,
    },
    is_imported_preview,
    merkle_cache::MerkleEvalCache,
    placement,
};

#[path = "graph_mech_features.rs"]
mod graph_mech_features;
#[path = "graph_sketch_form.rs"]
mod graph_sketch_form;

const SOLID_PORTS: &[&str] = &["solid", "base", "target", "body"];
const BOOLEAN_BASE_PORTS: &[&str] = &["base", "target", "left", "a"];
const BOOLEAN_TOOL_PORTS: &[&str] = &["tool", "right", "b"];
const TAG_GEOMETRY_PORTS: &[&str] = &["solid", "surface", "geometry"];

pub use graph_sketch_form::SketchEvalSummary;

/// Graph validation and serialization errors. Feature failures go in the report.
pub type GraphEvalError = IrError;

#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    pub input_hash: String,
    pub state: NodeState,
    /// Geometry available to downstream nodes, including rollback geometry.
    pub solid: Option<SolidShape>,
    pub topo_snapshot: Option<TopoSnapshot>,
    pub diagnostic: Option<String>,
    /// Native diagnostic from this node, not from its rollback geometry.
    pub detail: Option<NodeErrorDetail>,
    /// Non-fatal warnings emitted by this node (e.g. DFM undercuts, unverified physics, preview open shell).
    pub warnings: Vec<NodeErrorDetail>,
    /// Actionable next step when this node failed. Absent on success and cache hits.
    pub recovery_hint: Option<RecoveryHint>,
}

#[derive(Default)]
pub struct GraphEvalCache {
    snapshots: BTreeMap<NodeId, NodeSnapshot>,
    outputs: BTreeMap<NodeId, CachedOutput>,
}

impl fmt::Debug for GraphEvalCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphEvalCache")
            .field("snapshots", &self.snapshots)
            .finish()
    }
}

impl GraphEvalCache {
    pub fn get(&self, id: &NodeId) -> Option<&NodeSnapshot> {
        self.snapshots.get(id)
    }
}

/// `RecoveryHint` stores millimetre limits as `f64`, so this report is `PartialEq` only.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeEvalReport {
    pub status: NodeState,
    pub from_cache: bool,
    pub failed_topo_refs: Vec<String>,
    pub error: Option<String>,
    /// Kept alongside `status` so existing `NodeState` constructors and snapshots are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<NodeErrorDetail>,
    /// Present when a failure has a machine-readable next step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<RecoveryHint>,
}

#[derive(Debug, Default)]
pub struct GraphEvalReport {
    pub node_reports: BTreeMap<NodeId, NodeEvalReport>,
    pub output_solids: BTreeMap<NodeId, SolidShape>,
    /// L1 `CheckReport` computed once per evaluated solid output node.
    pub output_checks: BTreeMap<NodeId, CheckReport>,
    /// Placed world-space solids for evaluated `AssemblyComponent` nodes (local and eager `ExternalRef`).
    pub component_solids: BTreeMap<NodeId, SolidShape>,
    /// Part-local unplaced solids for evaluated `AssemblyComponent` nodes.
    pub local_component_solids: BTreeMap<NodeId, SolidShape>,
    /// Resolved `(origin_mm, rotation_deg)` placement per evaluated `AssemblyComponent` node.
    pub component_poses: BTreeMap<NodeId, ([f64; 3], [f64; 3])>,
    /// Part name per evaluated `AssemblyComponent` node.
    pub component_part_names: BTreeMap<NodeId, String>,
    /// Lightweight proxy bounds and metadata for `DeferredPayload` `ExternalRef` components.
    pub deferred_components: BTreeMap<NodeId, DeferredComponentSummary>,
    /// PMI annotations collected during graph evaluation.
    pub pmi_records: Vec<PmiRecord>,
    /// Non-fatal warnings emitted per node (e.g. `W_DFM_UNDERCUT_FACES`, `W_PHYSICS_UNVERIFIED`).
    pub warnings: BTreeMap<NodeId, Vec<NodeErrorDetail>>,
    #[cfg(feature = "implicit")]
    pub output_implicit_meshes: BTreeMap<NodeId, ImplicitEvalOutput>,
    #[cfg(feature = "subd")]
    pub output_subd_meshes: BTreeMap<NodeId, SubDMesh>,
    /// Deterministic 64-char hex Merkle hashes computed for every node in topological order.
    pub node_merkle_hashes: BTreeMap<NodeId, String>,
    /// Assembly poses and mate diagnostics keyed by node ID.
    pub assembly_solve_report: Option<SolveReport>,
    pub cache_hits: usize,
    pub cache_misses: usize,
    /// Number of cache misses, including suppressed nodes and failed operations.
    pub recomputed_nodes: usize,
    /// Sketch solve status for every evaluated `Sketch2D` node, including failures.
    pub sketch_reports: BTreeMap<NodeId, SketchEvalSummary>,
}

impl GraphEvalReport {
    /// Returns `true` when any experimental track (implicit field or SubD) produced outputs.
    pub fn has_experimental_outputs(&self) -> bool {
        #[cfg(feature = "implicit")]
        if !self.output_implicit_meshes.is_empty() {
            return true;
        }
        #[cfg(feature = "subd")]
        if !self.output_subd_meshes.is_empty() {
            return true;
        }
        false
    }
}

#[cfg(feature = "implicit")]
#[derive(Debug, Clone)]
pub struct ImplicitEvalOutput {
    pub mesh: ImplicitMesh,
    pub estimated_volume_mm3: f64,
    /// Estimated material volume divided by the volume of the clipping domain.
    pub relative_density: f64,
}

#[derive(Debug, Clone)]
struct PlacementMateCheck {
    id: String,
    mate_kind: String,
    comp_a: String,
    comp_b: String,
    axis: Option<[f64; 3]>,
    offset_mm: f64,
}

enum CachedOutput {
    #[cfg(feature = "implicit")]
    Scalar(Arc<dyn ScalarField>),
    #[cfg(feature = "implicit")]
    Tpms {
        base: TpmsField,
        thickness: Arc<dyn ScalarField>,
    },
    #[cfg(feature = "subd")]
    SubD {
        mesh: SubDMesh,
        faces: Vec<Vec<u32>>,
    },
    #[cfg(feature = "implicit")]
    Implicit(ImplicitEvalOutput),
    AssemblyComponent {
        component: AssemblyComponent,
        part_name: String,
        local_solid: Option<SolidShape>,
        extref_payload: Option<ResolvedExtRefPayload>,
    },
    AssemblyMate(AssemblyMate),
    PlacementMate(PlacementMateCheck),
    Sketch(Box<graph_sketch_form::SolvedSketch>),
    Pmi {
        target_solid: Option<NodeId>,
        record: PmiRecord,
    },
}

#[cfg(feature = "implicit")]
struct HalfField(Arc<dyn ScalarField>);

#[cfg(feature = "implicit")]
impl ScalarField for HalfField {
    fn eval(&self, p: [f64; 3]) -> f64 {
        0.5 * self.0.eval(p)
    }
}

pub struct GraphEvaluator<'kernel> {
    kernel: &'kernel dyn GeometryKernel,
    resolver: &'kernel dyn AssetResolver,
    next_import_instance: Cell<usize>,
    cache: GraphEvalCache,
    merkle_cache: MerkleEvalCache,
    explicit_kernel_fingerprint: Option<KernelFingerprint>,
    ext_ref_ctx: ExternalRefContext,
    kernel_identity: Option<(&'static str, usize)>,
}

impl<'kernel> GraphEvaluator<'kernel> {
    pub fn new(kernel: &'kernel dyn GeometryKernel) -> Self {
        Self::with_resolver(kernel, &NoopAssetResolver)
    }

    /// Construct a `GraphEvaluator` with an explicit workspace `AssetResolver` for filesystem imports.
    pub fn with_resolver(
        kernel: &'kernel dyn GeometryKernel,
        resolver: &'kernel dyn AssetResolver,
    ) -> Self {
        Self {
            kernel,
            resolver,
            next_import_instance: Cell::new(0),
            cache: GraphEvalCache::default(),
            merkle_cache: MerkleEvalCache::new(),
            explicit_kernel_fingerprint: None,
            ext_ref_ctx: ExternalRefContext::default(),
            kernel_identity: None,
        }
    }

    /// Construct a `GraphEvaluator` seeded with an existing node-level `MerkleEvalCache`.
    pub fn with_merkle_cache(
        kernel: &'kernel dyn GeometryKernel,
        merkle_cache: MerkleEvalCache,
    ) -> Self {
        Self {
            kernel,
            resolver: &NoopAssetResolver,
            next_import_instance: Cell::new(0),
            cache: GraphEvalCache::default(),
            merkle_cache,
            explicit_kernel_fingerprint: None,
            ext_ref_ctx: ExternalRefContext::default(),
            kernel_identity: None,
        }
    }

    /// Replace the `AssetResolver` used for filesystem `ImportAsset` nodes.
    pub fn set_resolver(&mut self, resolver: &'kernel dyn AssetResolver) {
        self.resolver = resolver;
    }

    pub fn cache(&self) -> &GraphEvalCache {
        &self.cache
    }

    /// Access the node-level Merkle geometry snapshot cache and its hit/miss telemetry.
    pub fn merkle_cache(&self) -> &MerkleEvalCache {
        &self.merkle_cache
    }

    /// Mutably access the node-level Merkle geometry snapshot cache.
    pub fn merkle_cache_mut(&mut self) -> &mut MerkleEvalCache {
        &mut self.merkle_cache
    }

    pub(crate) fn take_merkle_cache(&mut self) -> MerkleEvalCache {
        std::mem::take(&mut self.merkle_cache)
    }

    /// Override the `(kernel_id, kernel_version)` fingerprint used for Merkle cache keys.
    pub fn set_kernel_fingerprint(&mut self, fingerprint: KernelFingerprint) {
        self.explicit_kernel_fingerprint = Some(fingerprint);
    }

    /// Resolve the active `KernelFingerprint` for `kernel`.
    pub fn effective_kernel_fingerprint(&self, kernel: &dyn GeometryKernel) -> KernelFingerprint {
        self.explicit_kernel_fingerprint.clone().unwrap_or_else(|| {
            KernelFingerprint::new(kernel.backend_name(), env!("CARGO_PKG_VERSION"))
        })
    }

    /// Configure parent container and optional workspace sandbox root for resolving `ExternalRef` paths.
    pub fn set_external_ref_roots(
        &mut self,
        parent_container: Option<PathBuf>,
        workspace_root: Option<PathBuf>,
    ) {
        self.ext_ref_ctx.parent_container = parent_container.clone();
        self.ext_ref_ctx.workspace_root = workspace_root;
        self.ext_ref_ctx.active_chain.clear();
        if let Some(parent) = parent_container
            && let Ok(canon) = std::fs::canonicalize(parent)
        {
            self.ext_ref_ctx
                .active_chain
                .push(format!("path:{}", canon.display()));
        }
    }

    /// Pre-register an in-memory `AicadProject` for `ExternalRef` resolution (by `ref_id` or `container_path`).
    pub fn register_external_project(&mut self, key: impl Into<String>, project: AicadProject) {
        self.ext_ref_ctx.registered_projects.insert(key.into(), project);
    }

    pub(crate) fn set_external_ref_context(&mut self, ctx: ExternalRefContext) {
        self.ext_ref_ctx = ctx;
    }

    /// Evaluate the active branch of a `SqliteContainer`, hydrating from and persisting back to `node_brep_cache`,
    /// while resolving any inline `bundled_refs` and workspace-relative `ExternalRef`s.
    pub fn evaluate_sqlite_container(
        &mut self,
        container: &mut SqliteContainer,
    ) -> Result<GraphEvalReport, ContainerError> {
        self.evaluate_sqlite_branch(container, None)
    }

    /// Evaluate a named (or active) branch of a `SqliteContainer` with persistent `node_brep_cache` sync.
    pub fn evaluate_sqlite_branch(
        &mut self,
        container: &mut SqliteContainer,
        branch_opt: Option<&str>,
    ) -> Result<GraphEvalReport, ContainerError> {
        let active_branch = container.active_branch()?;
        let target_branch = branch_opt.unwrap_or(active_branch.as_str());
        let graph = container.load_branch_graph(target_branch)?;

        let mut blobs = BTreeMap::new();
        if !graph.blobs.is_empty() {
            if target_branch == active_branch {
                let reader = LazyProjectReader::open(container.path())?;
                for entry in graph.blobs.values() {
                    let bytes = reader.read_blob(&entry.relative_path)?;
                    blobs.insert(entry.relative_path.clone(), bytes);
                }
            } else {
                let project = container.load_project()?;
                blobs = project.blobs;
            }
        }

        let eager_needed = graph
            .external_refs
            .values()
            .any(|def| def.payload_policy == PayloadPolicy::Eager);
        let prev_ctx = self.ext_ref_ctx.clone();
        self.ext_ref_ctx
            .preload_from_sqlite(container, eager_needed)?;

        let kernel_fp = self.effective_kernel_fingerprint(self.kernel);
        let hashes = compute_node_merkle_hashes(&graph)?;
        self.merkle_cache
            .load_hashes_from_sqlite(container, &hashes, &kernel_fp)?;

        let eval_res = self.evaluate_graph_with_blobs(&graph, &blobs, self.kernel);
        self.ext_ref_ctx = prev_ctx;
        let report = eval_res?;

        let mut nominal_persistable_hashes = BTreeMap::new();
        for (node_id, hash) in &hashes {
            if report
                .node_reports
                .get(node_id)
                .is_some_and(|nr| nr.status == NodeState::Valid)
            {
                nominal_persistable_hashes.insert(node_id.clone(), hash.clone());
            }
        }
        let pinned_outputs: BTreeSet<&NodeId> = graph.outputs.iter().collect();
        self.merkle_cache.flush_hashes_to_sqlite(
            container,
            &nominal_persistable_hashes,
            &pinned_outputs,
            &kernel_fp,
        )?;

        Ok(report)
    }

    /// Evaluate any `.aicad` container at `container_path` (SQLite, sharded dir, single dir, or ZIP archive).
    pub fn evaluate_project_at_path(
        &mut self,
        container_path: &Path,
        workspace_root: Option<&Path>,
    ) -> Result<GraphEvalReport, ContainerError> {
        let prev_ws = self.ext_ref_ctx.workspace_root.clone();
        if let Some(ws) = workspace_root {
            self.ext_ref_ctx.workspace_root = Some(ws.to_path_buf());
        }
        let result = match detect_storage_kind(container_path)? {
            StorageKind::SqliteContainer => {
                let mut sqlite = SqliteContainer::open(container_path)?;
                self.evaluate_sqlite_container(&mut sqlite)
            }
            _ => {
                let project = AicadProject::load_any(container_path)?;
                let prev_ctx = self.ext_ref_ctx.clone();
                self.set_external_ref_roots(
                    Some(container_path.to_path_buf()),
                    self.ext_ref_ctx.workspace_root.clone(),
                );
                let report = self.evaluate_graph_with_blobs(
                    &project.graph,
                    &project.blobs,
                    self.kernel,
                );
                self.ext_ref_ctx = prev_ctx;
                report.map_err(ContainerError::from)
            }
        };
        self.ext_ref_ctx.workspace_root = prev_ws;
        result
    }

    /// Validate the graph, then evaluate every branch in dependency order.
    /// A feature failure is represented in its node report.
    pub fn evaluate(&mut self, graph: &CanonicalGraph) -> Result<GraphEvalReport, IrError> {
        self.evaluate_graph_with_blobs(graph, &BTreeMap::new(), self.kernel)
    }

    /// Evaluate with an explicit kernel and an empty blob map.
    pub fn evaluate_graph(
        &mut self,
        graph: &CanonicalGraph,
        kernel: &dyn GeometryKernel,
    ) -> Result<GraphEvalReport, GraphEvalError> {
        self.evaluate_graph_with_blobs(graph, &BTreeMap::new(), kernel)
    }

    /// Evaluate with in-memory blob payloads, looking up the manifest path before the handle.
    pub fn evaluate_graph_with_blobs(
        &mut self,
        graph: &CanonicalGraph,
        blobs: &BTreeMap<String, Vec<u8>>,
        kernel: &dyn GeometryKernel,
    ) -> Result<GraphEvalReport, GraphEvalError> {
        let eval_t0 = Instant::now();
        let graph_span = tracing::info_span!(
            "evaluate_graph",
            component = "cad-evaluator",
            operation = "evaluate_graph"
        );
        let _entered = graph_span.enter();

        // Formula values are written on a clone so `DimInput::Param` and sketch
        // dimensions read the current result. Cycle and dimension errors abort
        // the whole evaluation instead of keeping a stale literal.
        let mut resolved = graph.clone();
        if let Err(err) = resolved.evaluate_formulas() {
            tracing::error!(
                component = "cad-evaluator",
                operation = "evaluate_graph",
                duration_ms = eval_t0.elapsed().as_secs_f64() * 1000.0,
                error_type = "GraphEvalError",
                "{}",
                cad_observe::sanitize_message(&err.to_string())
            );
            return Err(err);
        }
        let graph = &resolved;

        let order = match execution_order(graph) {
            Ok(order) => order,
            Err(err) => {
                tracing::error!(
                    component = "cad-evaluator",
                    operation = "evaluate_graph",
                    duration_ms = eval_t0.elapsed().as_secs_f64() * 1000.0,
                    error_type = "GraphEvalError",
                    "{}",
                    cad_observe::sanitize_message(&err.to_string())
                );
                return Err(err);
            }
        };
        let identity = (
            kernel.backend_name(),
            std::ptr::from_ref(kernel).cast::<()>().addr(),
        );
        if self.kernel_identity.is_some_and(|old| old != identity) {
            self.cache = GraphEvalCache::default();
            if self.explicit_kernel_fingerprint.is_none() {
                self.merkle_cache = MerkleEvalCache::new();
            }
        }
        self.kernel_identity = Some(identity);

        let kernel_fp = self.effective_kernel_fingerprint(kernel);
        let merkle_hashes = compute_merkle_hashes_for_eval(graph, &order)?;
        let mut nominal_clean = BTreeSet::<NodeId>::new();

        let mut report = GraphEvalReport {
            node_merkle_hashes: merkle_hashes.clone(),
            ..GraphEvalReport::default()
        };

        for id in &order {
            let node_t0 = Instant::now();
            let node = &graph.nodes[&id.0];
            let upstream_nominal = all_upstream_nominal_clean(graph, node, &nominal_clean);
            let merkle_brep_eligible = node.state != NodeState::Suppressed
                && upstream_nominal
                && is_brep_merkle_cacheable_kind(&node.kind)
                && !(node.kind == NodeKind::ImportAsset && !node.inputs.contains_key("blob"));

            let input_hash = {
                let mut inputs = NodeInputs {
                    graph,
                    node,
                    cache: &self.cache,
                    blobs,
                    blob: None,
                    resolver: self.resolver,
                    next_import_instance: &self.next_import_instance,
                };
                inputs.blob = inputs.resolve_blob();
                inputs.input_hash(&self.ext_ref_ctx)?
            };

            let direct_cache_hit = self
                .cache
                .get(id)
                .is_some_and(|snapshot| snapshot.input_hash == input_hash);

            let from_cache = if direct_cache_hit {
                if merkle_brep_eligible
                    && self
                        .cache
                        .get(id)
                        .is_some_and(|snap| snap.state == NodeState::Valid && snap.solid.is_some())
                {
                    self.merkle_cache.record_hit();
                }
                report.cache_hits += 1;
                true
            } else if merkle_brep_eligible
                && let Some(merkle_hash) = merkle_hashes.get(id)
                && let Some((solid, topo_snapshot)) = self.merkle_cache.get(
                    merkle_hash,
                    &kernel_fp,
                    id,
                    node.kind == NodeKind::TagFaces,
                )
            {
                self.cache.snapshots.insert(
                    id.clone(),
                    NodeSnapshot {
                        input_hash,
                        state: NodeState::Valid,
                        solid: Some(solid),
                        topo_snapshot: Some(topo_snapshot),
                        diagnostic: None,
                        detail: None,
                        warnings: Vec::new(),
                        recovery_hint: None,
                    },
                );
                self.cache.outputs.remove(id);
                report.cache_hits += 1;
                true
            } else {
                let (snapshot, output) = {
                    let mut inputs = NodeInputs {
                        graph,
                        node,
                        cache: &self.cache,
                        blobs,
                        blob: None,
                        resolver: self.resolver,
                        next_import_instance: &self.next_import_instance,
                    };
                    inputs.blob = inputs.resolve_blob();
                    inputs.evaluate(
                        kernel,
                        input_hash,
                        &self.ext_ref_ctx,
                        &mut self.merkle_cache,
                    )
                };
                if merkle_brep_eligible
                    && snapshot.state == NodeState::Valid
                    && let (Some(merkle_hash), Some(solid), Some(topo)) = (
                        merkle_hashes.get(id),
                        snapshot.solid.as_ref(),
                        snapshot.topo_snapshot.as_ref(),
                    )
                {
                    self.merkle_cache.insert(
                        merkle_hash.clone(),
                        &kernel_fp,
                        id,
                        solid.clone(),
                        topo.clone(),
                    );
                }
                self.cache.snapshots.insert(id.clone(), snapshot);
                if let Some(output) = output {
                    self.cache.outputs.insert(id.clone(), output);
                } else {
                    self.cache.outputs.remove(id);
                }
                report.recomputed_nodes += 1;
                report.cache_misses += 1;
                false
            };

            let snapshot = &self.cache.snapshots[id];
            if upstream_nominal && snapshot.state == NodeState::Valid {
                nominal_clean.insert(id.clone());
            }

            let node_duration_ms = node_t0.elapsed().as_secs_f64() * 1000.0;
            match &snapshot.state {
                NodeState::Valid => {
                    tracing::debug!(
                        component = "cad-evaluator",
                        operation = "evaluate_node",
                        node_id = %id.0,
                        duration_ms = node_duration_ms,
                        from_cache,
                        "node evaluated"
                    );
                }
                NodeState::FailedWithRollback { reason, .. } => {
                    if let Some(detail) = snapshot.detail.as_ref() {
                        let msg = cad_observe::sanitize_message(&detail.message);
                        tracing::error!(
                            component = "cad-evaluator",
                            operation = detail.operation.as_deref().unwrap_or("evaluate_node"),
                            node_id = %id.0,
                            code = detail.code.as_str(),
                            native_status = detail.native_status,
                            duration_ms = node_duration_ms,
                            "{msg}"
                        );
                    } else {
                        let msg = cad_observe::sanitize_message(reason);
                        tracing::error!(
                            component = "cad-evaluator",
                            operation = "evaluate_node",
                            node_id = %id.0,
                            error_type = "NodeRollback",
                            duration_ms = node_duration_ms,
                            "{msg}"
                        );
                    }
                }
                NodeState::Suppressed | NodeState::Stale => {}
            }

            if !snapshot.warnings.is_empty() {
                report.warnings.insert(id.clone(), snapshot.warnings.clone());
            }

            let failed_topo_refs = match &snapshot.state {
                NodeState::FailedWithRollback {
                    failed_topo_refs, ..
                } => failed_topo_refs.clone(),
                _ => Vec::new(),
            };
            report.node_reports.insert(
                id.clone(),
                NodeEvalReport {
                    status: snapshot.state.clone(),
                    from_cache,
                    failed_topo_refs,
                    error: snapshot.diagnostic.clone(),
                    detail: snapshot.detail.clone(),
                    recovery_hint: snapshot.recovery_hint.clone(),
                },
            );
        }
        self.cache
            .snapshots
            .retain(|id, _| graph.nodes.contains_key(&id.0));
        self.cache
            .outputs
            .retain(|id, _| graph.nodes.contains_key(&id.0));

        if graph
            .nodes
            .values()
            .any(|node| node.kind == NodeKind::AssemblyMate && node.state != NodeState::Suppressed)
        {
            self.solve_assembly(&mut report);
        }
        self.populate_component_payloads(graph, kernel, &mut report);

        for (id, output) in &self.cache.outputs {
            if let CachedOutput::Sketch(solved) = output {
                report
                    .sketch_reports
                    .insert(id.clone(), solved.summary.clone());
            }
        }

        let mut pmi_entries: Vec<(Option<NodeId>, PmiRecord)> = Vec::new();
        for id in &order {
            if let Some(CachedOutput::Pmi {
                target_solid,
                record,
            }) = self.cache.outputs.get(id)
            {
                pmi_entries.push((target_solid.clone(), record.clone()));
            }
        }
        // Also collect thread PMI from evaluated solid nodes (e.g. standalone tapped-hole tools)
        // so thread annotations survive downstream boolean subtractions.
        let mut thread_pmi_records: Vec<PmiRecord> = Vec::new();
        for id in &order {
            if let Some(snap) = self.cache.snapshots.get(id)
                && snap.state == NodeState::Valid
                && let Some(solid) = snap.solid.as_ref()
            {
                for rec in &solid.pmi {
                    if !pmi_contains(&thread_pmi_records, rec) {
                        thread_pmi_records.push(rec.clone());
                    }
                }
            }
        }
        for (_, rec) in &pmi_entries {
            if !pmi_contains(&report.pmi_records, rec) {
                report.pmi_records.push(rec.clone());
            }
        }
        for rec in &thread_pmi_records {
            if !pmi_contains(&report.pmi_records, rec) {
                report.pmi_records.push(rec.clone());
            }
        }

        for id in &graph.outputs {
            if let Some(solid) = self.cache.snapshots[id].solid.as_ref() {
                let mut out_solid = solid.clone();
                if graph.outputs.len() == 1 {
                    for rec in &report.pmi_records {
                        if !pmi_contains(&out_solid.pmi, rec) {
                            out_solid.pmi.push(rec.clone());
                        }
                    }
                } else {
                    for (_, rec) in &pmi_entries {
                        if !pmi_contains(&out_solid.pmi, rec) {
                            out_solid.pmi.push(rec.clone());
                        }
                    }
                }
                let check = kernel.check_solid(&out_solid);
                let node = &graph.nodes[&id.0];
                let allow_open_shell = node.kind == NodeKind::ImportAsset
                    && node
                        .inputs
                        .get("allow_open_shell")
                        .and_then(|value| match value {
                            IrValue::Bool(flag) => Some(*flag),
                            _ => None,
                        })
                        .unwrap_or(false);
                if !allow_open_shell
                    && !crate::is_imported_preview(&out_solid)
                    && let Err(reason) = validate_l1_report(&out_solid, &check)
                    && let Some(node_report) = report.node_reports.get_mut(id)
                    && node_report.status == NodeState::Valid
                {
                    node_report.status = NodeState::FailedWithRollback {
                        reason: reason.clone(),
                        failed_topo_refs: Vec::new(),
                    };
                    node_report.error = Some(reason.clone());
                    node_report.detail = Some(NodeErrorDetail {
                        code: "E301_NON_WATERTIGHT".to_owned(),
                        operation: Some("l1_watertight_gate".to_owned()),
                        native_status: None,
                        message: reason,
                    });
                }
                report.output_checks.insert(id.clone(), check);
                report.output_solids.insert(id.clone(), out_solid);
            } else if let Some(placed) = report.component_solids.get(id) {
                report.output_solids.insert(id.clone(), placed.clone());
            }
            #[cfg(any(feature = "implicit", feature = "subd"))]
            match self.cache.outputs.get(id) {
                #[cfg(feature = "implicit")]
                Some(CachedOutput::Implicit(output)) => {
                    report
                        .output_implicit_meshes
                        .insert(id.clone(), output.clone());
                }
                #[cfg(feature = "subd")]
                Some(CachedOutput::SubD { mesh, .. }) => {
                    report.output_subd_meshes.insert(id.clone(), mesh.clone());
                }
                _ => {}
            }
        }
        let total_nodes = report.node_reports.len();
        let failed_nodes = report
            .node_reports
            .values()
            .filter(|nr| matches!(nr.status, NodeState::FailedWithRollback { .. }))
            .count();
        let duration_ms = eval_t0.elapsed().as_secs_f64() * 1000.0;
        tracing::info!(
            component = "cad-evaluator",
            operation = "evaluate_graph",
            duration_ms,
            total_nodes,
            failed_nodes,
            "evaluate_graph finished: total_nodes={total_nodes} failed_nodes={failed_nodes}"
        );
        Ok(report)
    }

    fn populate_component_payloads(
        &self,
        graph: &CanonicalGraph,
        kernel: &dyn GeometryKernel,
        report: &mut GraphEvalReport,
    ) {
        for (id, output) in &self.cache.outputs {
            let CachedOutput::AssemblyComponent {
                component,
                part_name,
                local_solid,
                extref_payload,
            } = output
            else {
                continue;
            };
            if graph
                .nodes
                .get(&id.0)
                .is_none_or(|n| n.state == NodeState::Suppressed)
            {
                continue;
            }
            let (origin, rotation) = report
                .assembly_solve_report
                .as_ref()
                .and_then(|solve| solve.solved_poses.get(&id.0))
                .map_or(
                    (component.pose.translation_mm, component.pose.rotation_deg),
                    |pose| (pose.translation_mm, pose.rotation_deg),
                );
            report.component_poses.insert(id.clone(), (origin, rotation));
            report
                .component_part_names
                .insert(id.clone(), part_name.clone());
            let world_mat = placement(origin, rotation);

            if let Some(payload) = extref_payload {
                match payload {
                    ResolvedExtRefPayload::EagerSolid(eager_solid) => {
                        let mut local = (**eager_solid).clone();
                        if !part_name.is_empty() {
                            local.part_name = part_name.clone();
                        }
                        report.local_component_solids.insert(id.clone(), local.clone());
                        let placed_res = if origin == [0.0; 3] && rotation == [0.0; 3] {
                            Ok(local)
                        } else {
                            kernel.transform(&local, world_mat)
                        };
                        match placed_res {
                            Ok(mut placed) => {
                                for face in &mut placed.faces {
                                    face.component_id.get_or_insert_with(|| id.0.clone());
                                }
                                for edge in &mut placed.edges {
                                    edge.component_id.get_or_insert_with(|| id.0.clone());
                                }
                                report.component_solids.insert(id.clone(), placed);
                            }
                            Err(err) => {
                                if let Some(nr) = report.node_reports.get_mut(id) {
                                    let reason = err.to_string();
                                    nr.status = NodeState::FailedWithRollback {
                                        reason: reason.clone(),
                                        failed_topo_refs: Vec::new(),
                                    };
                                    nr.error = Some(reason);
                                }
                            }
                        }
                    }
                    ResolvedExtRefPayload::DeferredProxy(summary) => {
                        let mut placed_summary = (**summary).clone();
                        placed_summary.proxy_bbox_mm =
                            transform_bbox(summary.proxy_bbox_mm, world_mat);
                        report
                            .deferred_components
                            .insert(id.clone(), placed_summary);
                    }
                }
            } else if let Some(local_solid) = local_solid {
                let mut local = local_solid.clone();
                if !part_name.is_empty() {
                    local.part_name = part_name.clone();
                }
                report.local_component_solids.insert(id.clone(), local);
            }
        }
    }

    fn solve_assembly(&self, report: &mut GraphEvalReport) {
        let mut components = Vec::new();
        let mut comp_lookup: BTreeMap<String, (&AssemblyComponent, Option<&SolidShape>)> =
            BTreeMap::new();
        let mut solver_mates = Vec::new();
        let mut placement_mates = Vec::new();
        for output in self.cache.outputs.values() {
            match output {
                CachedOutput::AssemblyComponent {
                    component,
                    local_solid,
                    extref_payload,
                    ..
                } => {
                    components.push(component.clone());
                    let solid = local_solid.as_ref().or_else(|| match extref_payload {
                        Some(ResolvedExtRefPayload::EagerSolid(s)) => Some(s.as_ref()),
                        _ => None,
                    });
                    comp_lookup.insert(component.id.clone(), (component, solid));
                }
                CachedOutput::AssemblyMate(mate) => solver_mates.push(mate.clone()),
                CachedOutput::PlacementMate(mate) => placement_mates.push(mate.clone()),
                _ => {}
            }
        }

        // Conflicts depend on the entire mate set. Keep them out of cached node results.
        let mut fail_mate = |id: &str, code: Option<&str>, reason: String| {
            let node_report = report
                .node_reports
                .get_mut(&NodeId::from(id))
                .expect("every registered mate has a node report");
            node_report.status = NodeState::FailedWithRollback {
                reason: reason.clone(),
                failed_topo_refs: Vec::new(),
            };
            node_report.error = Some(reason.clone());
            if let Some(code) = code {
                node_report.detail = Some(NodeErrorDetail {
                    code: code.to_owned(),
                    operation: Some("assembly_mate".to_owned()),
                    native_status: None,
                    message: reason,
                });
            }
        };

        if !solver_mates.is_empty() {
            // The cache is ordered by node ID, so the fallback is stable across evaluations.
            if !components.iter().any(|component| component.grounded)
                && let Some(first) = components.first_mut()
            {
                first.grounded = true;
            }
            let system = AssemblySystem::new(components.clone(), solver_mates);
            match system.solve(&SolveConfig::default()) {
                Ok(solve_report) => {
                    if !solve_report.converged {
                        for id in &solve_report.conflicting_mates {
                            let residual = solve_report.mate_residuals[id];
                            fail_mate(
                                id,
                                None,
                                format!("assembly mate did not converge (residual {residual})"),
                            );
                        }
                    }
                    report.assembly_solve_report = Some(solve_report);
                }
                Err(error) => {
                    for mate in &system.mates {
                        fail_mate(&mate.id, None, error.to_string());
                    }
                }
            }
        }

        if !placement_mates.is_empty() {
            let mut mate_residuals = BTreeMap::new();
            let mut conflicting_mates = Vec::new();
            for mate in &placement_mates {
                match verify_placement_mate(mate, &comp_lookup) {
                    Ok(()) => {
                        mate_residuals.insert(mate.id.clone(), 0.0);
                    }
                    Err(message) => {
                        mate_residuals.insert(mate.id.clone(), 1.0);
                        conflicting_mates.push(mate.id.clone());
                        fail_mate(&mate.id, Some("E208_INVALID_ASSEMBLY_MATE"), message.to_owned());
                    }
                }
            }
            if report.assembly_solve_report.is_none() {
                let solved_poses = components
                    .iter()
                    .map(|c| (c.id.clone(), c.pose.clone()))
                    .collect();
                let final_residual = mate_residuals.values().copied().fold(0.0_f64, f64::max);
                report.assembly_solve_report = Some(SolveReport {
                    converged: conflicting_mates.is_empty(),
                    iterations: 0,
                    final_residual,
                    solved_poses,
                    mate_residuals,
                    conflicting_mates,
                    free_dof: 0,
                });
            }
        }
    }
}

fn verify_placement_mate(
    mate: &PlacementMateCheck,
    comp_lookup: &BTreeMap<String, (&AssemblyComponent, Option<&SolidShape>)>,
) -> Result<(), &'static str> {
    let (Some((ca, sa_opt)), Some((cb, sb_opt))) =
        (comp_lookup.get(&mate.comp_a), comp_lookup.get(&mate.comp_b))
    else {
        return Err("mate requires two component instances");
    };
    let oa = ca.pose.translation_mm;
    let ra = ca.pose.rotation_deg;
    let ob = cb.pose.translation_mm;
    let rb = cb.pose.rotation_deg;
    let ma = placement(oa, ra);
    let mb = placement(ob, rb);
    const DIST_TOL: f64 = 1e-4;
    const ANG_TOL: f64 = 1e-8;
    let dot = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let sub = |u: [f64; 3], v: [f64; 3]| [u[0] - v[0], u[1] - v[1], u[2] - v[2]];

    if mate.mate_kind == "coaxial" {
        let direction = mate.axis.unwrap_or([0.0, 0.0, 1.0]);
        let norm = dot(direction, direction).sqrt();
        if !norm.is_finite() || norm < 1e-9 {
            return Err("coaxial mate axis must be nonzero");
        }
        let unit = direction.map(|x| x / norm);
        let da = ma.transform_dir(unit);
        let db = mb.transform_dir(unit);
        let d = sub(ob, oa);
        let axial = dot(d, da);
        let radial = sub(d, da.map(|x| x * axial));
        if dot(da, db).abs() >= 1.0 - ANG_TOL && dot(radial, radial) <= DIST_TOL.powi(2) {
            return Ok(());
        }
        return Err("component placement axes are not coaxial");
    }

    if mate.mate_kind == "coincident" {
        let (Some(sa), Some(sb)) = (sa_opt, sb_opt) else {
            return Err("mate requires two component instances with geometry");
        };
        let offset = mate.offset_mm;
        for fa in sa
            .faces
            .iter()
            .filter(|f| f.surface_type == SurfaceType::Plane)
        {
            let na = ma.transform_dir(fa.normal_hint);
            let ca_pt = ma.transform_point(fa.center_hint);
            for fb in sb
                .faces
                .iter()
                .filter(|f| f.surface_type == SurfaceType::Plane)
            {
                let nb = mb.transform_dir(fb.normal_hint);
                if dot(na, nb) <= -1.0 + ANG_TOL
                    && (dot(na, sub(mb.transform_point(fb.center_hint), ca_pt)) - offset).abs()
                        <= DIST_TOL
                {
                    return Ok(());
                }
            }
        }
        return Err("no opposing planar face pair satisfies the coincident mate offset");
    }

    Ok(())
}

fn pmi_contains(records: &[PmiRecord], candidate: &PmiRecord) -> bool {
    records.iter().any(|existing| {
        existing.kind == candidate.kind
            && existing.label == candidate.label
            && existing.tolerance_mm.map(f64::to_bits) == candidate.tolerance_mm.map(f64::to_bits)
            && existing.datum == candidate.datum
            && existing.target_tags == candidate.target_tags
    })
}

fn is_brep_merkle_cacheable_kind(kind: &NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::ParameterRef { .. }
            | NodeKind::Sketch2D
            | NodeKind::SubDFromBlob
            | NodeKind::SubDToNurbs
            | NodeKind::FieldSdf
            | NodeKind::FieldTpms { .. }
            | NodeKind::FieldRamp
            | NodeKind::FieldFromFeaBlob
            | NodeKind::ImplicitLatticeInfill
            | NodeKind::AssemblyComponent
            | NodeKind::AssemblyMate
            | NodeKind::AssertGate
            | NodeKind::StdLibCall
    )
}

fn all_upstream_nominal_clean(
    graph: &CanonicalGraph,
    node: &IrNode,
    nominal_clean: &BTreeSet<NodeId>,
) -> bool {
    let mut clean = true;
    for input in node.inputs.values() {
        visit_values_with_ext_refs(graph, input, &mut |value| {
            if let IrValue::NodeRef { node_id, .. } = value
                && !nominal_clean.contains(node_id)
            {
                clean = false;
            }
        });
    }
    clean
}

fn compute_merkle_hashes_for_eval(
    graph: &CanonicalGraph,
    order: &[NodeId],
) -> Result<BTreeMap<NodeId, String>, IrError> {
    if let Ok(hashes) = compute_node_merkle_hashes(graph) {
        return Ok(hashes);
    }
    let mut hashes = BTreeMap::new();
    for node_id in order {
        let node = &graph.nodes[&node_id.0];
        let parameter = match &node.kind {
            NodeKind::ParameterRef { param_id } => graph.parameters.get(&param_id.0),
            _ => None,
        };
        let mut upstream = BTreeMap::new();
        for value in node.inputs.values() {
            visit_values(value, &mut |inner| {
                if let IrValue::NodeRef { node_id: up_id, .. } = inner
                    && let Some(up_hash) = hashes.get(up_id)
                {
                    upstream.insert(&up_id.0, up_hash);
                }
            });
        }
        let bytes = serde_json::to_vec(&(
            "cad-ir:node-merkle:eval-fallback:v1",
            &graph.units,
            &node.kind,
            node.output_type,
            &node.kernel_op_version,
            parameter,
            &node.inputs,
            upstream,
        ))?;
        hashes.insert(node_id.clone(), format!("{:x}", Sha256::digest(&bytes)));
    }
    Ok(hashes)
}

// Validate the existing IR contracts first, then restore the evaluator's
// additional connections as dynamic call ports for the final dependency order.
fn execution_order(graph: &CanonicalGraph) -> Result<Vec<NodeId>, IrError> {
    let connections: Vec<_> = graph
        .nodes
        .iter()
        .flat_map(|(key, node)| {
            node.inputs.iter().filter_map(move |(port, value)| {
                let IrValue::NodeRef { node_id, .. } = value else {
                    return None;
                };
                let expected = extra_input_types(&node.kind, port)?;
                Some((key, port.as_str(), value, node_id, expected))
            })
        })
        .collect();
    if connections.is_empty() {
        return cad_ir::validate_graph(graph);
    }

    let mut validation_graph = graph.clone();
    for &(key, port, value, _, _) in &connections {
        let node = validation_graph.nodes.get_mut(key).unwrap();
        node.inputs.remove(port);
        if port == "domain" && !SOLID_PORTS.iter().any(|p| node.inputs.contains_key(*p)) {
            node.inputs.insert("solid".into(), value.clone());
        }
    }
    cad_ir::validate_graph(&validation_graph)?;

    for (key, port, value, upstream, expected) in connections {
        let actual = graph
            .nodes
            .get(&upstream.0)
            .ok_or_else(|| IrError::DanglingNode(upstream.clone()))?
            .output_type;
        if !expected.contains(&actual) {
            return Err(IrError::TypeMismatch {
                location: format!("node `{key}` input `{port}`"),
                expected: expected.to_vec(),
                actual: Some(actual),
            });
        }

        let node = validation_graph.nodes.get_mut(key).unwrap();
        node.kind = NodeKind::StdLibCall;
        node.inputs.insert(port.into(), value.clone());
    }
    cad_ir::validate_graph(&validation_graph)
}

fn extra_input_types(kind: &NodeKind, port: &str) -> Option<&'static [IrType]> {
    use IrType::{
        Angle, Bool, Component, Int, Length, Scalar, ScalarField, Solid, String, Vec3Dir,
        Vec3Length,
    };
    use NodeKind::{
        AssemblyComponent, AssemblyMate, AssertGate, FieldFromFeaBlob, FieldRamp, FieldTpms,
        ImplicitLatticeInfill, SubDFromBlob, SubDToNurbs,
    };

    Some(match (kind, port) {
        (AssertGate, "solid") | (ImplicitLatticeInfill, "domain") => &[Solid],
        (
            FieldRamp,
            "start_mm" | "start" | "end_mm" | "end" | "min_val" | "min_value" | "min" | "max_val"
            | "max_value" | "max",
        )
        | (ImplicitLatticeInfill, "skin_thickness_mm") => &[Length, Scalar, Int],
        (FieldTpms { .. }, "period_mm") => &[Length, Scalar, Int, Vec3Length, Vec3Dir],
        (FieldTpms { .. }, "thickness_mm" | "thickness") => &[Length, Scalar, Int, ScalarField],
        (FieldTpms { .. }, "thickness_field") => &[ScalarField],
        (ImplicitLatticeInfill, "resolution") | (SubDFromBlob, "levels") => &[Int, Scalar],
        (FieldFromFeaBlob, "min_stress_mpa" | "max_stress_mpa") => &[Scalar, Int],
        (FieldFromFeaBlob, "min_thickness_mm" | "max_thickness_mm") => &[Length, Scalar, Int],
        (SubDToNurbs, "thickness_mm" | "thickness") => &[Length, Scalar, Int],
        (AssemblyComponent, "origin" | "rotation") => &[Vec3Length, Vec3Dir],
        (AssemblyComponent, "grounded") => &[Bool],
        (AssemblyMate, "a" | "b") => &[Component],
        (AssemblyMate, "origin_a" | "origin_b") => &[Vec3Length, Vec3Dir],
        (AssemblyMate, "axis_a" | "axis_b") => &[Vec3Dir, Vec3Length],
        (AssemblyMate, "offset" | "target") => &[Angle, Length, Scalar, Int],
        (AssemblyMate, "anti_aligned") => &[Bool],
        (AssemblyMate, "weight") => &[Scalar, Int],
        (AssemblyMate, "mate_kind") => &[String],
        _ => return None,
    })
}

#[cfg(feature = "implicit")]
const TPMS_THICKNESS_PORTS: &[&str] = &["thickness_mm", "thickness_field", "thickness"];
const Z_AXIS: [f64; 3] = [0.0, 0.0, 1.0];
// Bound the size of the intermediate geometry built during extraction and subdivision.
#[cfg(feature = "implicit")]
const MAX_LATTICE_CELLS: usize = 128 * 128 * 128;
#[cfg(feature = "subd")]
const MAX_SUBD_REFINED_FACES: usize = 1_000_000;

#[cfg(not(all(feature = "implicit", feature = "subd")))]
fn feature_disabled_error(node_kind: &str, feature: &str) -> FeatureError {
    let message = format!(
        "node kind `{node_kind}` requires optional `cad-evaluator` feature `{feature}` (or `experimental`) to be enabled"
    );
    FeatureError {
        reason: message.clone(),
        detail: Some(NodeErrorDetail {
            code: "E_FEATURE_DISABLED".into(),
            operation: Some(node_kind.to_owned()),
            native_status: None,
            message,
        }),
        sketch: None,
        recovery_hint: None,
    }
}

type FeatureResult<T> = Result<T, String>;

struct FeatureError {
    reason: String,
    detail: Option<NodeErrorDetail>,
    /// Present when a sketch was solved (or failed to solve) before the node failed.
    sketch: Option<Box<graph_sketch_form::SolvedSketch>>,
    recovery_hint: Option<RecoveryHint>,
}

impl From<String> for FeatureError {
    fn from(reason: String) -> Self {
        Self {
            reason,
            detail: None,
            sketch: None,
            recovery_hint: None,
        }
    }
}

impl From<&str> for FeatureError {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

impl From<KernelOperationError> for FeatureError {
    fn from(error: KernelOperationError) -> Self {
        let reason = error.to_string();
        let detail = match error {
            KernelOperationError::Native(native) => Some(NodeErrorDetail {
                code: "R012_KERNEL_NATIVE".into(),
                operation: Some(native.operation),
                native_status: Some(native.status_code),
                message: native.message,
            }),
            KernelOperationError::Kernel(KernelError::UnsupportedMixedBoolean) => {
                Some(NodeErrorDetail {
                    code: "E507_UNSUPPORTED_MIXED_BOOLEAN".into(),
                    operation: Some("boolean".into()),
                    native_status: None,
                    message: reason.clone(),
                })
            }
            KernelOperationError::Kernel(_) => None,
        };
        Self {
            reason,
            detail,
            sketch: None,
            recovery_hint: None,
        }
    }
}

struct ResolvedBlob<'a> {
    entry: &'a BlobManifestEntry,
    payload: Option<(&'a [u8], String)>,
}

struct NodeInputs<'a> {
    graph: &'a CanonicalGraph,
    node: &'a IrNode,
    cache: &'a GraphEvalCache,
    blobs: &'a BTreeMap<String, Vec<u8>>,
    blob: Option<ResolvedBlob<'a>>,
    resolver: &'a dyn AssetResolver,
    next_import_instance: &'a Cell<usize>,
}

impl<'a> NodeInputs<'a> {
    fn resolve_blob(&self) -> Option<ResolvedBlob<'a>> {
        if !matches!(
            self.node.kind,
            NodeKind::SubDFromBlob | NodeKind::FieldFromFeaBlob | NodeKind::ImportAsset
        ) {
            return None;
        }
        let IrValue::BlobRef(handle) = self.input(&["blob"])? else {
            return None;
        };
        let entry = self.graph.blobs.get(&handle.0)?;
        let payload = self
            .blobs
            .get(&entry.relative_path)
            .or_else(|| self.blobs.get(&handle.0))
            .map(|bytes| (bytes.as_slice(), format!("{:x}", Sha256::digest(bytes))));
        Some(ResolvedBlob { entry, payload })
    }

    fn input_hash(&self, ext_ref_ctx: &ExternalRefContext) -> Result<String, IrError> {
        let mut upstream = BTreeMap::new();
        let mut ext_refs_used = BTreeMap::new();
        for input in self.node.inputs.values() {
            visit_values_with_ext_refs(self.graph, input, &mut |value| match value {
                IrValue::NodeRef { node_id, .. } => {
                    if let Some(snap) = self.cache.snapshots.get(node_id) {
                        upstream.insert(node_id, &snap.input_hash);
                    }
                }
                IrValue::ExternalRef(ref_id) => {
                    if let Some(def) = self.graph.external_refs.get(&ref_id.0) {
                        ext_refs_used.insert(&ref_id.0, def);
                    }
                }
                _ => {}
            });
        }
        let parameter = match &self.node.kind {
            NodeKind::ParameterRef { param_id } => Some(&self.graph.parameters[&param_id.0].value),
            _ => None,
        };
        let (bound_parameters, sketch_blobs) = self.feature_binding_hash();
        let ext_env_stamp = (!ext_refs_used.is_empty()).then(|| {
            (
                &ext_ref_ctx.parent_container,
                &ext_ref_ctx.workspace_root,
                ext_ref_ctx.registered_projects.keys().collect::<Vec<_>>(),
                ext_ref_ctx
                    .bundled_refs
                    .iter()
                    .map(|(k, v)| (k, &v.entry.container_sha256, v.project.is_some()))
                    .collect::<Vec<_>>(),
            )
        });
        let serialized = serde_json::to_vec(&(
            &self.graph.graph_id,
            &self.node.kind,
            &self.node.inputs,
            self.node.output_type,
            &self.node.name,
            &self.node.kernel_op_version,
            self.node.state == NodeState::Suppressed,
            parameter,
            upstream,
            ext_refs_used,
            ext_env_stamp,
            bound_parameters,
            sketch_blobs,
        ))?;
        let mut hash = Sha256::new();
        hash.update(serialized);
        if let Some(blob) = &self.blob {
            let digest = blob.payload.as_ref().map(|(_, digest)| digest);
            hash.update(serde_json::to_vec(&(blob.entry, digest))?);
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    fn evaluate(
        &self,
        kernel: &dyn GeometryKernel,
        input_hash: String,
        ext_ref_ctx: &ExternalRefContext,
        merkle_cache: &mut MerkleEvalCache,
    ) -> (NodeSnapshot, Option<CachedOutput>) {
        let mut snapshot = NodeSnapshot {
            input_hash,
            state: NodeState::Valid,
            solid: None,
            topo_snapshot: None,
            diagnostic: None,
            detail: None,
            warnings: Vec::new(),
            recovery_hint: None,
        };

        let mut pass_through = |state, diagnostic| {
            snapshot.state = state;
            snapshot.diagnostic = diagnostic;
            if let Some(upstream) = self.primary().filter(|upstream| upstream.solid.is_some()) {
                snapshot.solid.clone_from(&upstream.solid);
                snapshot.topo_snapshot.clone_from(&upstream.topo_snapshot);
            }
        };

        if self.node.state == NodeState::Suppressed {
            pass_through(NodeState::Suppressed, None);
            return (snapshot, None);
        }

        let eval_result = self.execute(kernel, ext_ref_ctx, merkle_cache);

        match eval_result {
            Ok(Evaluated::PassThrough) => pass_through(NodeState::Valid, None),
            Ok(Evaluated::PassThroughWithWarnings(warnings)) => {
                pass_through(NodeState::Valid, None);
                snapshot.warnings = warnings;
            }
            Ok(Evaluated::Solid(solid, topo_snapshot)) => {
                self.finalize_solid_snapshot(solid, topo_snapshot, Vec::new(), &mut snapshot);
            }
            Ok(Evaluated::SolidWithWarnings(solid, topo_snapshot, warnings)) => {
                self.finalize_solid_snapshot(solid, topo_snapshot, warnings, &mut snapshot);
            }
            Ok(Evaluated::Track(output)) => return (snapshot, Some(output)),
            Err(FeatureError {
                reason,
                detail,
                sketch,
                recovery_hint,
            }) => {
                pass_through(
                    NodeState::FailedWithRollback {
                        reason: reason.clone(),
                        failed_topo_refs: self.failed_topo_refs(),
                    },
                    Some(reason),
                );
                snapshot.detail = detail;
                snapshot.recovery_hint = recovery_hint;
                let output = sketch.map(CachedOutput::Sketch);
                return (snapshot, output);
            }
        }
        (snapshot, None)
    }

    fn finalize_solid_snapshot(
        &self,
        mut solid: SolidShape,
        topo_snapshot: Option<TopoSnapshot>,
        warnings: Vec<NodeErrorDetail>,
        snapshot: &mut NodeSnapshot,
    ) {
        if let Some(tag) = self.node.metadata.get("dsl.thread_tag") {
            let clean_spec = tag.strip_prefix("tapped_hole:").unwrap_or(tag).trim();
            if !clean_spec.is_empty()
                && !solid
                    .pmi
                    .iter()
                    .any(|p| p.kind == "pmi::thread" && p.label == clean_spec)
            {
                for face in &mut solid.faces {
                    if face.surface_type == SurfaceType::Cylinder
                        && !face.provenance_tags.iter().any(|t| t == tag)
                    {
                        face.provenance_tags.push(tag.clone());
                    }
                }
                solid.pmi.push(PmiRecord {
                    kind: "pmi::thread".to_owned(),
                    label: clean_spec.to_owned(),
                    tolerance_mm: None,
                    datum: None,
                    target_tags: vec![tag.clone()],
                    span: Span::default(),
                });
            }
        }
        snapshot.topo_snapshot =
            Some(topo_snapshot.unwrap_or_else(|| self.propagate_output_topology(&solid)));
        snapshot.solid = Some(solid);
        snapshot.warnings = warnings;
    }

    /// Parameter values named by a feature spec, plus sketch blob bytes.
    ///
    /// Legacy nodes have no `feature_spec` port, so both collections stay empty
    /// and an unchanged graph still cache-hits.
    fn feature_binding_hash(&self) -> (BTreeMap<String, IrValue>, Vec<(String, String)>) {
        let Some(IrValue::String(_)) = self.node.inputs.get(cad_ir::FEATURE_SPEC_PORT) else {
            return (BTreeMap::new(), Vec::new());
        };
        let Ok(spec) = self.node.try_feature_spec() else {
            return (BTreeMap::new(), Vec::new());
        };
        let bound_parameters = spec
            .referenced_params()
            .into_iter()
            .filter_map(|id| {
                self.graph
                    .parameters
                    .get(&id.0)
                    .map(|def| (id.0, def.value.clone()))
            })
            .collect();
        let mut sketch_blobs = Vec::new();
        if let cad_ir::FeatureSpec::Sketch2D(sketch) = spec {
            for handle in sketch.referenced_blobs() {
                let digest = self
                    .blob_payload(&handle)
                    .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
                    .or_else(|| {
                        self.graph
                            .blobs
                            .get(&handle.0)
                            .map(|entry| entry.sha256.clone())
                    })
                    .unwrap_or_default();
                sketch_blobs.push((handle.0, digest));
            }
        }
        (bound_parameters, sketch_blobs)
    }

    fn blob_payload(&self, handle: &cad_ir::BlobHandle) -> Option<&'a [u8]> {
        let entry = self.graph.blobs.get(&handle.0)?;
        self.blobs
            .get(&entry.relative_path)
            .or_else(|| self.blobs.get(&handle.0))
            .map(|bytes| bytes.as_slice())
    }

    fn execute(
        &self,
        kernel: &dyn GeometryKernel,
        ext_ref_ctx: &ExternalRefContext,
        merkle_cache: &mut MerkleEvalCache,
    ) -> Result<Evaluated, FeatureError> {
        let span = Span::default();
        if self.raw(&[cad_ir::FEATURE_SPEC_PORT]).is_some()
            && let Some(result) = self.execute_typed_feature(kernel)
        {
            return result;
        }
        match self.node.kind {
            NodeKind::ParameterRef { .. } => Ok(Evaluated::PassThrough),
            NodeKind::AssemblyComponent => self
                .assembly_component(kernel, ext_ref_ctx, merkle_cache)
                .map_err(Into::into),
            NodeKind::AssemblyMate => self.assembly_mate().map_err(Into::into),
            NodeKind::AssertGate => {
                let warnings = self.assert_gate(kernel)?;
                if warnings.is_empty() {
                    Ok(Evaluated::PassThrough)
                } else {
                    Ok(Evaluated::PassThroughWithWarnings(warnings))
                }
            }
            NodeKind::StdLibCall => self.eval_stdlib_call(),
            NodeKind::TagFaces => self.tag_faces(),
            NodeKind::PrimitiveBox => graph_sketch_form::eval_spec_box(self, kernel),
            NodeKind::PrimitiveCylinder => graph_sketch_form::eval_spec_cylinder(self, kernel),
            NodeKind::Sketch2D => graph_sketch_form::eval_sketch(self),
            NodeKind::Extrude => graph_sketch_form::eval_extrude(self, kernel),
            NodeKind::Revolve => graph_sketch_form::eval_revolve(self, kernel),
            NodeKind::Sweep => graph_sketch_form::eval_sweep(self, kernel),
            NodeKind::LoftGuided => graph_sketch_form::eval_loft(self, kernel),
            NodeKind::BooleanUnion | NodeKind::BooleanSubtract | NodeKind::BooleanIntersect => {
                graph_mech_features::eval_boolean(self, kernel)
            }
            NodeKind::ImplicitSmoothBlend => self.eval_smooth_blend(kernel),
            NodeKind::Fillet => graph_mech_features::eval_fillet(self, kernel),
            NodeKind::Chamfer => graph_mech_features::eval_chamfer(self, kernel),
            NodeKind::Shell => graph_mech_features::eval_shell(self, kernel),
            NodeKind::Hole => graph_mech_features::eval_hole(self, kernel),
            NodeKind::PatternLinear => graph_mech_features::eval_linear_pattern(self, kernel),
            NodeKind::PatternPolar => graph_mech_features::eval_polar_pattern(self, kernel),
            NodeKind::Mirror => graph_mech_features::eval_mirror(self, kernel),
            NodeKind::Draft => graph_mech_features::eval_draft(self, kernel),
            NodeKind::DirectFaceEdit => graph_mech_features::eval_direct_face_edit(self, kernel),
            NodeKind::FieldRamp => {
                #[cfg(feature = "implicit")]
                return self.field_ramp().map_err(Into::into);
                #[cfg(not(feature = "implicit"))]
                return Err(feature_disabled_error("FieldRamp", "implicit"));
            }
            NodeKind::FieldTpms { family } => {
                #[cfg(feature = "implicit")]
                return self.field_tpms(family).map_err(Into::into);
                #[cfg(not(feature = "implicit"))]
                {
                    let _ = family;
                    return Err(feature_disabled_error("FieldTpms", "implicit"));
                }
            }
            NodeKind::FieldFromFeaBlob => {
                #[cfg(feature = "implicit")]
                return self.field_from_fea_blob().map_err(Into::into);
                #[cfg(not(feature = "implicit"))]
                return Err(feature_disabled_error("FieldFromFeaBlob", "implicit"));
            }
            NodeKind::ImplicitLatticeInfill => {
                #[cfg(feature = "implicit")]
                return self.implicit_lattice().map_err(Into::into);
                #[cfg(not(feature = "implicit"))]
                return Err(feature_disabled_error("ImplicitLatticeInfill", "implicit"));
            }
            NodeKind::SubDFromBlob => {
                #[cfg(feature = "subd")]
                return self.subd_from_blob().map_err(Into::into);
                #[cfg(not(feature = "subd"))]
                return Err(feature_disabled_error("SubDFromBlob", "subd"));
            }
            NodeKind::SubDToNurbs => {
                #[cfg(feature = "subd")]
                return self.subd_to_nurbs(span).map_err(Into::into);
                #[cfg(not(feature = "subd"))]
                return Err(feature_disabled_error("SubDToNurbs", "subd"));
            }
            NodeKind::ImportAsset => self.import_asset(kernel, span),
            ref kind => Err(format!("unsupported graph node kind: {kind:?}").into()),
        }
    }

    fn eval_smooth_blend(&self, kernel: &dyn GeometryKernel) -> Result<Evaluated, FeatureError> {
        let values = self.boolean_operand_values();
        if values.is_empty() {
            return Err("smooth blend requires solid operands".into());
        }
        let solids: Vec<SolidShape> = values
            .into_iter()
            .map(|value| {
                self.upstream(value)
                    .and_then(|snapshot| snapshot.solid.clone())
                    .ok_or_else(|| FeatureError::from("smooth blend operand has no effective solid"))
            })
            .collect::<Result<_, _>>()?;
        let solid = kernel.try_boolean_union_detailed(solids, &self.node.name)?;
        if solid.faces.is_empty() || !valid_bbox(solid.csg.bbox()) {
            return Err("kernel returned an empty solid or invalid bounding box".into());
        }
        Ok(Evaluated::Solid(solid, None))
    }

    fn execute_typed_feature(
        &self,
        kernel: &dyn GeometryKernel,
    ) -> Option<Result<Evaluated, FeatureError>> {
        Some(match self.node.kind {
            NodeKind::PrimitiveBox => graph_sketch_form::eval_spec_box(self, kernel),
            NodeKind::PrimitiveCylinder => graph_sketch_form::eval_spec_cylinder(self, kernel),
            NodeKind::Sketch2D => graph_sketch_form::eval_sketch(self),
            NodeKind::Extrude => graph_sketch_form::eval_extrude(self, kernel),
            NodeKind::Revolve => graph_sketch_form::eval_revolve(self, kernel),
            NodeKind::Sweep => graph_sketch_form::eval_sweep(self, kernel),
            NodeKind::LoftGuided => graph_sketch_form::eval_loft(self, kernel),
            NodeKind::BooleanUnion | NodeKind::BooleanSubtract | NodeKind::BooleanIntersect => {
                graph_mech_features::eval_boolean(self, kernel)
            }
            NodeKind::Fillet => graph_mech_features::eval_fillet(self, kernel),
            NodeKind::Chamfer => graph_mech_features::eval_chamfer(self, kernel),
            NodeKind::Shell => graph_mech_features::eval_shell(self, kernel),
            NodeKind::Hole => graph_mech_features::eval_hole(self, kernel),
            NodeKind::PatternLinear => graph_mech_features::eval_linear_pattern(self, kernel),
            NodeKind::PatternPolar => graph_mech_features::eval_polar_pattern(self, kernel),
            NodeKind::Mirror => graph_mech_features::eval_mirror(self, kernel),
            NodeKind::Draft => graph_mech_features::eval_draft(self, kernel),
            NodeKind::DirectFaceEdit => graph_mech_features::eval_direct_face_edit(self, kernel),
            NodeKind::StdLibCall => graph_mech_features::eval_std_fastener_hole(self, kernel),
            _ => return None,
        })
    }

    fn assembly_component(
        &self,
        kernel: &dyn GeometryKernel,
        ext_ref_ctx: &ExternalRefContext,
        merkle_cache: &mut MerkleEvalCache,
    ) -> FeatureResult<Evaluated> {
        let ext_ref_val = self.input(&["external_ref"]);
        let spec_pose = self
            .node
            .try_feature_spec()
            .ok()
            .and_then(|spec| match spec {
                cad_ir::FeatureSpec::ComponentInstance {
                    translation_mm,
                    rotation_deg,
                    grounded,
                    ..
                } => Some((translation_mm, rotation_deg, grounded)),
                _ => None,
            });
        let translation_mm = match self.input(&["origin"]) {
            Some(origin) => vector(origin)?,
            None if spec_pose.is_some() => spec_pose.unwrap().0,
            None if ext_ref_val.is_some() => [0.0; 3],
            None => return Err("missing `origin`".into()),
        };
        let rotation_deg = match self.input(&["rotation"]) {
            Some(rot) => vector(rot)?,
            None if spec_pose.is_some() => spec_pose.unwrap().1,
            None => [0.0; 3],
        };
        let grounded = if self.input(&["grounded"]).is_some() {
            self.optional_bool("grounded")?
        } else {
            spec_pose.map_or(false, |(_, _, g)| g)
        };

        let mut part_name = self
            .node
            .metadata
            .get("dsl.part_name")
            .cloned()
            .unwrap_or_default();
        let local_solid = match self.raw(&["geometry"]) {
            Some(IrValue::NodeRef { node_id, .. }) => {
                if part_name.is_empty()
                    && let Some(geom_node) = self.graph.nodes.get(&node_id.0)
                {
                    part_name = geom_node
                        .metadata
                        .get("dsl.part_name")
                        .cloned()
                        .unwrap_or_else(|| geom_node.name.clone());
                }
                self.cache.get(node_id).and_then(|snap| snap.solid.clone())
            }
            _ => None,
        };
        if part_name.is_empty() {
            part_name = local_solid
                .as_ref()
                .map(|s| s.part_name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| self.node.name.clone());
        }

        let extref_payload = match ext_ref_val {
            Some(IrValue::ExternalRef(ref_id)) => {
                let def = self
                    .graph
                    .external_refs
                    .get(&ref_id.0)
                    .ok_or_else(|| format!("unknown external reference `{ref_id}`"))?;
                Some(evaluate_external_ref_component(
                    self.graph,
                    &self.node.id,
                    &self.node.metadata,
                    self.cache,
                    def,
                    ext_ref_ctx,
                    merkle_cache,
                    kernel,
                )?)
            }
            Some(_) => return Err("`external_ref`: expected an ExternalRef value".into()),
            None => None,
        };

        Ok(Evaluated::Track(CachedOutput::AssemblyComponent {
            component: AssemblyComponent {
                id: self.node.id.0.clone(),
                pose: RigidPose {
                    translation_mm,
                    rotation_deg,
                },
                grounded,
            },
            part_name,
            local_solid,
            extref_payload,
        }))
    }

    fn assembly_mate(&self) -> FeatureResult<Evaluated> {
        if self
            .node
            .metadata
            .get("dsl.placement_mate")
            .map(String::as_str)
            == Some("true")
        {
            let mate_kind = self.string(&["mate_kind", "mate"])?;
            let comp_a = self.component_id(&["a", "left"])?;
            let comp_b = self.component_id(&["b", "right"])?;
            let axis = self
                .input(&["axis", "axis_a"])
                .map(vector)
                .transpose()?;
            let offset_mm = self.optional_number(&["offset", "target"], IrType::Length, 0.0)?;
            return Ok(Evaluated::Track(CachedOutput::PlacementMate(
                PlacementMateCheck {
                    id: self.node.id.0.clone(),
                    mate_kind: mate_kind.to_owned(),
                    comp_a,
                    comp_b,
                    axis,
                    offset_mm,
                },
            )));
        }
        let mate_kind = self.string(&["mate_kind"])?;
        let a = self.mate_anchor(&["a", "left"], "origin_a", "axis_a")?;
        let b = self.mate_anchor(&["b", "right"], "origin_b", "axis_b")?;
        let kind = match mate_kind {
            "coaxial" => AssemblyMateKind::Coaxial { a, b },
            "coincident" => AssemblyMateKind::Coincident {
                a,
                b,
                offset_mm: self.optional_number(&["offset", "target"], IrType::Length, 0.0)?,
                anti_aligned: self.optional_bool("anti_aligned")?,
            },
            "distance" => AssemblyMateKind::Distance {
                a,
                b,
                target_mm: self.optional_number(&["target", "offset"], IrType::Length, 0.0)?,
            },
            "parallel" => AssemblyMateKind::Parallel { a, b },
            "angle" => AssemblyMateKind::Angle {
                a,
                b,
                target_deg: self.optional_number(&["target", "offset"], IrType::Angle, 0.0)?,
            },
            other => return Err(format!("unsupported assembly mate kind `{other}`")),
        };
        Ok(Evaluated::Track(CachedOutput::AssemblyMate(AssemblyMate {
            id: self.node.id.0.clone(),
            kind,
            weight: self.optional_number(&["weight"], IrType::Scalar, 1.0)?,
        })))
    }

    fn eval_stdlib_call(&self) -> Result<Evaluated, FeatureError> {
        if let Some(func) = self.optional_string(&["function"])? {
            return match func {
                "force::distributed" | "field::stress_driven" => Ok(Evaluated::PassThrough),
                "pmi::datum"
                | "pmi::perpendicularity"
                | "pmi::flatness"
                | "pmi::tolerance"
                | "pmi::thread" => self.eval_pmi_call(func),
                other => Err(format!("unsupported StdLibCall `{other}`").into()),
            };
        }
        Err("unsupported StdLibCall".into())
    }

    fn eval_pmi_call(&self, func: &str) -> Result<Evaluated, FeatureError> {
        let empty = BTreeMap::new();
        let args = match self.raw(&["arguments"]) {
            Some(IrValue::Map(map)) => map,
            _ => &empty,
        };
        let get_str = |keys: &[&str]| -> Option<String> {
            for key in keys {
                if let Some(val) = args.get(*key)
                    && let IrValue::String(s) = self.literal(val)
                {
                    return Some(s.clone());
                }
            }
            None
        };
        let get_num = |keys: &[&str]| -> Option<f64> {
            for key in keys {
                if let Some(val) = args.get(*key) {
                    match self.literal(val) {
                        IrValue::Quantity(q) => return Some(q.canonical_value()),
                        IrValue::Scalar(x) => return Some(*x),
                        IrValue::Int(x) => return Some(*x as f64),
                        _ => {}
                    }
                }
            }
            None
        };

        let label = get_str(&["0", "label", "spec", "datum"]).unwrap_or_else(|| "PMI".to_owned());
        let tolerance_mm = get_num(&["flatness", "tol", "tolerance"]);
        let datum = get_str(&["datum"]);

        let mut target_solid: Option<NodeId> = None;
        let mut target_tags: Vec<String> = Vec::new();
        if let Some(receiver) = args.get("receiver") {
            match receiver {
                IrValue::Map(map) => {
                    let query_pair = if map.get("dsl_topo_query") == Some(&IrValue::Bool(true)) {
                        match (map.get("root"), map.get("expr")) {
                            (
                                Some(IrValue::NodeRef { node_id, .. }),
                                Some(IrValue::TopoRefExpr(expr) | IrValue::String(expr)),
                            ) => Some((node_id, expr)),
                            _ => None,
                        }
                    } else if let Some(IrValue::Map(query_map)) = map.get("dsl_topo_query") {
                        match (query_map.get("root"), query_map.get("expr")) {
                            (
                                Some(IrValue::NodeRef { node_id, .. }),
                                Some(IrValue::TopoRefExpr(expr) | IrValue::String(expr)),
                            ) => Some((node_id, expr)),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if let Some((node_id, expr)) = query_pair {
                        target_solid = Some(node_id.clone());
                        if let Some(snap) = self.cache.get(node_id)
                            && let (Some(solid), Some(topo)) =
                                (snap.solid.as_ref(), snap.topo_snapshot.as_ref())
                        {
                            let disambiguated = match serde_json::from_str::<TopoRef>(expr) {
                                Ok(TopoRef::Query(q)) => {
                                    !q.tie_breakers.is_empty() || q.require_unique
                                }
                                _ => true,
                            };
                            if let Ok(indices) = graph_mech_features::face_indices_for(
                                topo,
                                solid,
                                std::slice::from_ref(expr),
                                false,
                            ) {
                                if func == "pmi::datum" && indices.len() > 1 && !disambiguated {
                                    let message = "pmi::datum requires a disambiguated single face (use .max_by/.min_by)".to_owned();
                                    return Err(FeatureError {
                                        reason: message.clone(),
                                        detail: Some(NodeErrorDetail {
                                            code: "E201_AMBIGUOUS_TOPOLOGY".to_owned(),
                                            operation: Some("pmi::datum".to_owned()),
                                            native_status: None,
                                            message,
                                        }),
                                        sketch: None,
                                        recovery_hint: None,
                                    });
                                }
                                for idx in indices {
                                    if let Some(face) = solid.faces.get(idx) {
                                        target_tags.extend(face.provenance_tags.iter().cloned());
                                    }
                                }
                            }
                        }
                    }
                }
                IrValue::NodeRef { node_id, .. } => {
                    target_solid = Some(node_id.clone());
                }
                _ => {}
            }
        }

        Ok(Evaluated::Track(CachedOutput::Pmi {
            target_solid,
            record: PmiRecord {
                kind: func.to_owned(),
                label,
                tolerance_mm,
                datum,
                target_tags,
                span: Span::default(),
            },
        }))
    }

    fn mate_anchor(
        &self,
        component_ports: &[&str],
        origin_port: &str,
        axis_port: &str,
    ) -> FeatureResult<MateAnchor> {
        let component_id = self.component_id(component_ports)?;
        let origin_local_mm = self
            .input(&[origin_port])
            .map(vector)
            .transpose()?
            .unwrap_or([0.0; 3]);
        let axis_local = self
            .input(&[axis_port])
            .map(vector)
            .transpose()?
            .unwrap_or(Z_AXIS);
        // Match the solver's minimum axis length before registering the mate.
        let axis_length_squared: f64 = axis_local.into_iter().map(|value| value * value).sum();
        if !axis_length_squared.is_finite() || axis_length_squared < 1e-24 {
            return Err(format!(
                "`{axis_port}` must have a finite, nonzero direction"
            ));
        }
        Ok(MateAnchor {
            component_id,
            origin_local_mm,
            axis_local,
        })
    }

    fn component_id(&self, ports: &[&str]) -> FeatureResult<String> {
        let value = self
            .raw(ports)
            .ok_or_else(|| format!("missing `{}`", ports[0]))?;
        let IrValue::NodeRef { node_id, .. } = value else {
            return Err(format!(
                "`{}` must reference an assembly component",
                ports[0]
            ));
        };
        if !matches!(
            self.cache.outputs.get(node_id),
            Some(CachedOutput::AssemblyComponent { .. })
        ) {
            return Err(format!(
                "`{}` requires an available assembly component `{node_id}`",
                ports[0]
            ));
        }
        Ok(node_id.0.clone())
    }

    fn optional_bool(&self, port: &str) -> FeatureResult<bool> {
        match self.input(&[port]) {
            Some(IrValue::Bool(value)) => Ok(*value),
            Some(_) => Err(format!("`{port}`: expected a boolean")),
            None => Ok(false),
        }
    }

    fn optional_string(&self, ports: &[&str]) -> FeatureResult<Option<&str>> {
        match self.input(ports) {
            Some(IrValue::String(value)) => Ok(Some(value.as_str())),
            Some(_) => Err(format!("`{}`: expected a string", ports[0])),
            None => Ok(None),
        }
    }

    fn import_asset(
        &self,
        kernel: &dyn GeometryKernel,
        span: Span,
    ) -> Result<Evaluated, FeatureError> {
        if let Some(blob) = self.blob.as_ref() {
            let entry = blob.entry;
            if entry.kind != BlobKind::Import {
                return Err(format!(
                    "blob `{}` has a non-Import manifest kind",
                    entry.handle
                )
                .into());
            }
            let (bytes, digest) = blob
                .payload
                .as_ref()
                .ok_or_else(|| format!("missing import asset blob `{}`", entry.relative_path))?;
            if bytes.len() as u64 != entry.byte_len || *digest != entry.sha256 {
                return Err(format!(
                    "import asset blob `{}` does not match its manifest length or SHA-256",
                    entry.relative_path
                )
                .into());
            }

            let explicit_format = self
                .optional_string(&["format"])?
                .or_else(|| self.node.metadata.get("format").map(String::as_str));
            let source_name = self
                .optional_string(&["source_file", "path"])?
                .or_else(|| self.node.metadata.get("source_file").map(String::as_str))
                .or_else(|| self.node.metadata.get("path").map(String::as_str));
            let format = resolve_import_asset_format(explicit_format, source_name, bytes)?;

            let default_ext = match format {
                ImportFormat::Step => "step",
                ImportFormat::Brep => "brep",
                ImportFormat::Iges => "iges",
                ImportFormat::Stl => "stl",
                ImportFormat::ThreeMf => "3mf",
                ImportFormat::Obj => "obj",
                ImportFormat::Gltf => "glb",
            };
            let virtual_path = source_name
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("{}.{default_ext}", self.node.id.0));
            let deflection_mm =
                self.optional_number(&["deflection_mm", "deflection"], IrType::Length, 0.05)?;
            if !deflection_mm.is_finite() || deflection_mm <= 0.0 {
                return Err("`deflection_mm`: expected a positive finite length".into());
            }
            let options = ImportOptions {
                format: Some(format),
                sha256: Some(entry.sha256.clone()),
                deflection_mm,
                component: self.optional_string(&["component"])?.map(ToOwned::to_owned),
                allow_open_shell: self.optional_bool("allow_open_shell")?,
                heal: self.optional_bool("heal")?,
                ..ImportOptions::default()
            };

            struct BlobAssetResolver<'a> {
                bytes: &'a [u8],
            }
            impl AssetResolver for BlobAssetResolver<'_> {
                fn resolve(&self, _path: &str) -> Result<Vec<u8>, AssetResolveError> {
                    Ok(self.bytes.to_vec())
                }
            }

            let resolver = BlobAssetResolver { bytes };
            let evaluated =
                evaluate_import_with_kernel(&resolver, &virtual_path, &options, span, kernel)
                    .map_err(|diag| FeatureError {
                        reason: format!("[{}] {}", diag.code, diag.message),
                        detail: Some(NodeErrorDetail {
                            code: diag.code,
                            operation: Some("import_asset".to_owned()),
                            native_status: None,
                            message: diag.message,
                        }),
                        sketch: None,
                        recovery_hint: None,
                    })?;
            let solid = evaluated.solid;
            if solid.faces.is_empty() || !valid_bbox(solid.csg.bbox()) {
                return Err("kernel returned an empty solid or invalid bounding box".into());
            }
            return Ok(Evaluated::Solid(solid, None));
        }

        let Some(path) = self
            .optional_string(&["path", "source_file"])?
            .map(str::trim)
            .filter(|p| !p.is_empty())
        else {
            return Err("missing import asset blob reference or `path`".into());
        };
        let raw_format = self
            .optional_string(&["format"])?
            .or_else(|| self.node.metadata.get("format").map(String::as_str));
        let mesh_family = match raw_format {
            Some("mesh") => Some(true),
            Some("step") => Some(false),
            _ => None,
        };
        let explicit_format = match raw_format {
            None | Some("mesh" | "step") => None,
            Some(other) => Some(resolve_import_asset_format(Some(other), Some(path), &[])?),
        };
        let source_unit = match self.optional_string(&["source_unit"])? {
            Some(raw_unit) => match raw_unit.parse::<LengthUnit>() {
                Ok(unit) => Some(unit),
                Err(err) => {
                    let msg = err.to_string();
                    return Err(FeatureError {
                        reason: msg.clone(),
                        detail: Some(NodeErrorDetail {
                            code: "E504_UNSUPPORTED_IMPORT_FORMAT".to_owned(),
                            operation: Some("import_asset".to_owned()),
                            native_status: None,
                            message: msg,
                        }),
                        sketch: None,
                        recovery_hint: None,
                    });
                }
            },
            None => None,
        };
        let deflection_mm =
            self.optional_number(&["deflection", "deflection_mm"], IrType::Length, 0.05)?;
        let cluster_analytic_faces = !matches!(
            self.input(&["cluster_analytic_faces"]),
            Some(IrValue::Bool(false))
        );
        let sewing_tol_mm = if self.raw(&["sewing_tol", "sewing_tol_mm"]).is_some() {
            Some(self.optional_number(&["sewing_tol", "sewing_tol_mm"], IrType::Length, 0.001)?)
        } else {
            None
        };
        let options = ImportOptions {
            format: explicit_format,
            sha256: self.optional_string(&["sha256"])?.map(ToOwned::to_owned),
            source_unit,
            deflection_mm,
            component: self.optional_string(&["component"])?.map(ToOwned::to_owned),
            cluster_analytic_faces,
            allow_open_shell: self.optional_bool("allow_open_shell")?,
            heal: self.optional_bool("heal")?,
            sewing_tol_mm,
        };
        let import_span = if span.end_byte > span.start_byte {
            span
        } else {
            self.node_span()
        };
        let mut imported = evaluate_import_for_family(
            self.resolver,
            path,
            &options,
            mesh_family,
            import_span,
            kernel,
        )
                .map_err(|diag| FeatureError {
                    reason: diag.message.clone(),
                    detail: Some(NodeErrorDetail {
                        code: diag.code,
                        operation: Some("import_asset".to_owned()),
                        native_status: None,
                        message: diag.message,
                    }),
                    sketch: None,
                    recovery_hint: None,
                })?;

        let index = self.next_import_instance.get();
        self.next_import_instance.set(index + 1);
        let prefix = if imported.diagnostics.is_empty() {
            "import"
        } else {
            "import_preview"
        };
        for face in &mut imported.solid.faces {
            face.face_id = format!("{prefix}_{index}_{}", face.face_id);
        }
        for edge in &mut imported.solid.edges {
            edge.edge_id = format!("{prefix}_{index}_{}", edge.edge_id);
        }
        kernel.scope_import_instance(&mut imported.solid);
        let warnings: Vec<NodeErrorDetail> = imported
            .diagnostics
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| NodeErrorDetail {
                code: d.code,
                operation: Some("import_asset".to_owned()),
                native_status: None,
                message: d.message,
            })
            .collect();
        let bbox = imported.solid.csg.bbox();
        let bbox_ok = if options.allow_open_shell {
            bbox.min
                .into_iter()
                .zip(bbox.max)
                .all(|(min, max)| min.is_finite() && max.is_finite() && max >= min)
        } else {
            valid_bbox(bbox)
        };
        if imported.solid.faces.is_empty() || !bbox_ok {
            return Err("kernel returned an empty solid or invalid bounding box".into());
        }
        if warnings.is_empty() {
            Ok(Evaluated::Solid(imported.solid, None))
        } else {
            Ok(Evaluated::SolidWithWarnings(imported.solid, None, warnings))
        }
    }

    fn optional_number(
        &self,
        ports: &[&str],
        value_type: IrType,
        default: f64,
    ) -> FeatureResult<f64> {
        match self.input(ports) {
            Some(IrValue::Quantity(quantity)) if quantity.ir_type() == value_type => {
                Ok(quantity.canonical_value())
            }
            Some(IrValue::Scalar(value)) => Ok(*value),
            Some(IrValue::Int(value)) => Ok(*value as f64),
            None => Ok(default),
            _ => Err(format!("`{}`: expected a {value_type:?} value", ports[0])),
        }
    }

    #[cfg(feature = "implicit")]
    fn field_ramp(&self) -> FeatureResult<Evaluated> {
        let direction = self
            .input(&["direction"])
            .map(vector)
            .transpose()?
            .unwrap_or([1.0, 0.0, 0.0]);
        let start = self.length(&["start_mm", "start"])?;
        let end = self.length(&["end_mm", "end"])?;
        let min = self.length(&["min_val", "min_value", "min"])?;
        let max = self.length(&["max_val", "max_value", "max"])?;
        let span = end - start;
        if !direction.into_iter().all(f64::is_finite)
            || direction == [0.0; 3]
            || !span.is_finite()
            || span == 0.0
            || !min.is_finite()
            || !max.is_finite()
        {
            return Err(
                "field ramp requires a finite nonzero direction and span, and finite values".into(),
            );
        }
        let ramp = FieldRamp::linear(direction, start, end, min, max);
        Ok(Evaluated::Track(CachedOutput::Scalar(Arc::new(ramp))))
    }

    #[cfg(feature = "implicit")]
    fn field_tpms(&self, family: TpmsFamily) -> FeatureResult<Evaluated> {
        let period = self
            .input(&["period_mm", "period", "cell_size"])
            .ok_or("missing `period_mm`")?;
        let period_mm = match period {
            IrValue::Vec3(_) | IrValue::List(_) => vector(period)?,
            _ => [length(period)?; 3],
        };
        for period in period_mm {
            positive(period, "TPMS period")?;
            if !(std::f64::consts::TAU / period).is_finite() {
                return Err("TPMS period is too small".into());
            }
        }
        let thickness = self
            .input(TPMS_THICKNESS_PORTS)
            .ok_or("missing `thickness_mm`")?;
        let thickness: Arc<dyn ScalarField> = match thickness {
            IrValue::NodeRef { node_id, .. } => match self.cache.outputs.get(node_id) {
                Some(CachedOutput::Scalar(field)) => Arc::clone(field),
                _ => return Err("TPMS thickness requires an available scalar field".into()),
            },
            value => Arc::new(ConstantField(positive(length(value)?, "TPMS thickness")?)),
        };
        Ok(Evaluated::Track(CachedOutput::Tpms {
            base: TpmsField::new(family, period_mm),
            thickness,
        }))
    }

    #[cfg(feature = "implicit")]
    fn field_from_fea_blob(&self) -> FeatureResult<Evaluated> {
        if self.node.output_type == IrType::VectorField {
            return Err("FEA field blob codec produces a scalar field, not a vector field".into());
        }
        let blob = self
            .blob
            .as_ref()
            .ok_or("missing FEA field blob reference")?;
        if blob.entry.kind != BlobKind::Field {
            return Err(format!(
                "blob {} has a non-field manifest kind",
                blob.entry.handle
            ));
        }
        let (bytes, digest) = blob
            .payload
            .as_ref()
            .ok_or_else(|| format!("missing FEA field blob {}", blob.entry.relative_path))?;
        if bytes.len() as u64 != blob.entry.byte_len || *digest != blob.entry.sha256 {
            return Err(format!(
                "FEA field blob {} does not match its manifest length or SHA-256",
                blob.entry.relative_path
            ));
        }

        let field = decode_voxel_field_blob(bytes).map_err(|error| error.to_string())?;
        let mapping_ports = [
            "min_stress_mpa",
            "max_stress_mpa",
            "min_thickness_mm",
            "max_thickness_mm",
        ];
        let present = mapping_ports
            .iter()
            .filter(|port| self.raw(&[port]).is_some())
            .count();
        let field: Arc<dyn ScalarField> = match present {
            0 => Arc::new(field),
            4 => Arc::new(
                StressDrivenThicknessField::try_new(
                    Arc::new(field),
                    self.optional_number(&["min_stress_mpa"], IrType::Scalar, 0.0)?,
                    self.optional_number(&["max_stress_mpa"], IrType::Scalar, 0.0)?,
                    self.optional_number(&["min_thickness_mm"], IrType::Length, 0.0)?,
                    self.optional_number(&["max_thickness_mm"], IrType::Length, 0.0)?,
                )
                .map_err(|error| error.to_string())?,
            ),
            _ => {
                return Err(
                    "FEA field thickness mapping requires all four stress/thickness limits".into(),
                );
            }
        };
        Ok(Evaluated::Track(CachedOutput::Scalar(field)))
    }

    #[cfg(feature = "implicit")]
    fn implicit_lattice(&self) -> FeatureResult<Evaluated> {
        let domain = self.primary_solid()?;
        let Some(IrValue::NodeRef { node_id, .. }) = self.raw(&["field", "lattice"]) else {
            return Err("implicit lattice requires a TPMS field connection".into());
        };
        let Some(CachedOutput::Tpms { base, thickness }) = self.cache.outputs.get(node_id) else {
            return Err("implicit lattice requires an available TPMS field".into());
        };

        let bbox = domain.csg.bbox();
        if !valid_bbox(bbox) {
            return Err("implicit lattice domain has an invalid bounding box".into());
        }
        let half_extents = std::array::from_fn(|axis| (bbox.max[axis] - bbox.min[axis]) * 0.5);
        let center = std::array::from_fn(|axis| bbox.min[axis] + half_extents[axis]);
        let (domain_field, domain_volume): (Box<dyn ScalarField>, f64) = match &domain.csg {
            CsgNode::Cylinder {
                axis,
                radius,
                height,
                ..
            } if axis[0] == 0.0 && axis[1] == 0.0 && axis[2].abs() == 1.0 => (
                Box::new(CylinderSdf::new(center, *radius, height * 0.5)),
                std::f64::consts::PI * radius * radius * height,
            ),
            // Other solids use their axis-aligned bounding box as the clipping domain.
            _ => (
                Box::new(BoxSdf::new(center, half_extents)),
                bbox.min
                    .into_iter()
                    .zip(bbox.max)
                    .map(|(min, max)| max - min)
                    .product(),
            ),
        };
        let domain_volume = positive(domain_volume, "implicit lattice domain volume")?;
        let sheet = TpmsSheetField::new(*base, Box::new(HalfField(Arc::clone(thickness))));
        let mut lattice = LatticeInfillSdf::new(domain_field, sheet);
        if let Some(value) = self.input(&["skin_thickness_mm", "wall_thickness"]) {
            let skin = length(value)?;
            if !skin.is_finite() || skin < 0.0 {
                return Err("skin thickness must be finite and nonnegative".into());
            }
            lattice = lattice.with_skin(skin);
        }

        let resolution = self.resolution()?;
        let mesh = cad_implicit::extract_isosurface(&lattice, bbox.min, bbox.max, resolution);
        if mesh.indices.is_empty() {
            return Err(
                "implicit lattice produced no isosurface at the requested resolution".into(),
            );
        }
        let estimated_volume_mm3 =
            cad_implicit::estimate_volume_mm3(&lattice, bbox.min, bbox.max, resolution);
        if !estimated_volume_mm3.is_finite() {
            return Err("implicit lattice volume estimate is non-finite".into());
        }
        Ok(Evaluated::Track(CachedOutput::Implicit(
            ImplicitEvalOutput {
                mesh,
                estimated_volume_mm3,
                relative_density: (estimated_volume_mm3 / domain_volume).clamp(0.0, 1.0),
            },
        )))
    }

    #[cfg(feature = "subd")]
    fn subd_from_blob(&self) -> FeatureResult<Evaluated> {
        let blob = self.blob.as_ref().ok_or("missing SubD blob reference")?;
        let entry = blob.entry;
        if entry.kind != BlobKind::SubD {
            return Err(format!(
                "blob `{}` has a non-SubD manifest kind",
                entry.handle
            ));
        }
        let (bytes, digest) = blob
            .payload
            .as_ref()
            .ok_or_else(|| format!("missing SubD blob `{}`", entry.relative_path))?;
        if bytes.len() as u64 != entry.byte_len || *digest != entry.sha256 {
            return Err(format!(
                "SubD blob `{}` does not match its manifest length or SHA-256",
                entry.relative_path
            ));
        }

        let levels = match self.input(&["levels"]) {
            Some(value) => {
                whole_number(value).ok_or("SubD levels must be a nonnegative integer")?
            }
            None => 1,
        };
        let cage = decode_subd_blob(bytes).map_err(|error| error.to_string())?;
        if levels > 0 {
            // The first step creates one quad per input corner. Each later step quadruples that count.
            let quads = cage.faces.iter().map(Vec::len).sum::<usize>();
            let output_faces = 4_usize
                .checked_pow(levels - 1)
                .and_then(|factor| quads.checked_mul(factor));
            if !output_faces.is_some_and(|faces| faces <= MAX_SUBD_REFINED_FACES) {
                return Err(format!(
                    "SubD subdivision exceeds {MAX_SUBD_REFINED_FACES} output faces"
                ));
            }
        }
        let cage = subdivide(&cage, levels).map_err(|error| error.to_string())?;
        let mesh = to_triangle_mesh(&cage);
        Ok(Evaluated::Track(CachedOutput::SubD {
            mesh,
            faces: cage.faces,
        }))
    }

    #[cfg(feature = "subd")]
    fn subd_to_nurbs(&self, span: Span) -> FeatureResult<Evaluated> {
        let IrValue::NodeRef { node_id, .. } = self.raw(&["subd"]).ok_or("missing SubD input")?
        else {
            return Err("SubD input must reference a node".into());
        };
        let Some(CachedOutput::SubD { mesh, faces }) = self.cache.outputs.get(node_id) else {
            return Err("SubD input has no available mesh".into());
        };
        // Surface outputs default to a zero-thickness face. Solid outputs
        // default to a one-millimeter extrusion for the native solid contract.
        let default_thickness = match self.node.output_type {
            IrType::Surface => 0.0,
            IrType::Solid => 1.0,
            _ => return Err("SubD-to-NURBS output must be a surface or solid".into()),
        };
        let thickness = self.optional_number(
            &["thickness_mm", "thickness"],
            IrType::Length,
            default_thickness,
        )?;
        if !thickness.is_finite() || thickness < 0.0 {
            return Err("SubD-to-NURBS thickness must be finite and nonnegative".into());
        }
        match (self.node.output_type, thickness) {
            (IrType::Surface, value) if value > 0.0 => {
                return Err("surface SubD-to-NURBS output requires zero thickness".into());
            }
            (IrType::Solid, value) if value <= 0.0 => {
                return Err("solid SubD-to-NURBS output requires positive thickness".into());
            }
            _ => {}
        }
        let spec = quad_grid_surface_spec(mesh, faces, thickness)?;
        let solid = quad_grid_to_bspline_solid(&spec, span).map_err(|error| error.to_string())?;
        if solid.faces.is_empty() {
            return Err("NURBS conversion returned an empty shape".into());
        }
        Ok(Evaluated::Solid(solid, None))
    }

    fn assert_gate(
        &self,
        kernel: &dyn GeometryKernel,
    ) -> Result<Vec<NodeErrorDetail>, FeatureError> {
        let coded_err = |code: &str, op: &str, message: String| FeatureError {
            reason: message.clone(),
            detail: Some(NodeErrorDetail {
                code: code.to_owned(),
                operation: Some(op.to_owned()),
                native_status: None,
                message,
            }),
            sketch: None,
            recovery_hint: None,
        };

        if let Some(IrValue::String(raw_spec)) = self.raw(&[cad_ir::FEATURE_SPEC_PORT])
            && let Ok(cad_ir::FeatureSpec::DesignAssertion { target, kind, .. }) =
                serde_json::from_str::<cad_ir::FeatureSpec>(raw_spec)
        {
            if let cad_ir::DesignAssertionKind::Custom {
                ref expression,
                ref params,
            } = kind
            {
                match expression.as_str() {
                    "assert::bbox" => {
                        let solid = self.assertion_target_solid(target.as_ref())?;
                        let sz = solid.csg.bbox().size();
                        let max_sz = if let Some(val) = self.input(&["max_size"]) {
                            Some(vector(val)?)
                        } else if let Some(raw) = params.get("max_size_mm") {
                            serde_json::from_str::<[f64; 3]>(raw).ok()
                        } else {
                            None
                        };
                        if let Some(max_sz) = max_sz
                            && (sz[0] > max_sz[0] + 1e-3
                                || sz[1] > max_sz[1] + 1e-3
                                || sz[2] > max_sz[2] + 1e-3)
                        {
                            return Err(coded_err(
                                "E303_BBOX_ASSERTION_FAILED",
                                "assert::bbox",
                                format!(
                                    "bounding box [{:.2}, {:.2}, {:.2}] mm exceeds max_size [{:.2}, {:.2}, {:.2}] mm",
                                    sz[0], sz[1], sz[2], max_sz[0], max_sz[1], max_sz[2]
                                ),
                            ));
                        }
                        return Ok(Vec::new());
                    }
                    "assert::dfm_accessible" => {
                        let solid = self.assertion_target_solid(target.as_ref())?;
                        let axes: Vec<[f64; 3]> = if let Some(axes_val) = self.input(&["axes"]) {
                            match axes_val {
                                IrValue::List(items) => items
                                    .iter()
                                    .filter_map(|item| vector(self.literal(item)).ok())
                                    .collect(),
                                single => vector(single).into_iter().collect(),
                            }
                        } else if let Some(raw) = params.get("axes") {
                            serde_json::from_str::<Vec<[f64; 3]>>(raw).unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        let chk = kernel.check_solid(solid);
                        if !chk.watertight {
                            return Err(coded_err(
                                "E301_NON_WATERTIGHT",
                                "assert::dfm_accessible",
                                "DFM accessibility check failed on non-watertight solid".to_owned(),
                            ));
                        }
                        return match dfm_tool_accessibility(solid, &axes) {
                            DfmAccessibility::Accessible => Ok(Vec::new()),
                            DfmAccessibility::Inaccessible { faces, area_ratio } => {
                                let shown: Vec<&str> =
                                    faces.iter().take(8).map(String::as_str).collect();
                                let more = faces.len().saturating_sub(shown.len());
                                let message = format!(
                                    "{} face(s) ({:.1}% of surface area) are occluded from every declared tool axis: {}{}",
                                    faces.len(),
                                    area_ratio * 100.0,
                                    shown.join(", "),
                                    if more > 0 {
                                        format!(" (+{more} more)")
                                    } else {
                                        String::new()
                                    }
                                );
                                if area_ratio > DFM_MAX_BLOCKED_AREA_RATIO {
                                    Err(coded_err(
                                        "E304_DFM_INACCESSIBLE",
                                        "assert::dfm_accessible",
                                        message,
                                    ))
                                } else {
                                    Ok(vec![NodeErrorDetail {
                                        code: "W_DFM_UNDERCUT_FACES".to_owned(),
                                        operation: Some("assert::dfm_accessible".to_owned()),
                                        native_status: None,
                                        message,
                                    }])
                                }
                            }
                            DfmAccessibility::Skipped(reason) => Ok(vec![NodeErrorDetail {
                                code: "W_DFM_ACCESSIBILITY_SKIPPED".to_owned(),
                                operation: Some("assert::dfm_accessible".to_owned()),
                                native_status: None,
                                message: format!(
                                    "DFM tool-axis accessibility was not verified: {reason}"
                                ),
                            }]),
                        };
                    }
                    "assert::no_interference" => {
                        let min_clearance_mm = self
                            .input(&["min_clearance"])
                            .and_then(|v| length(v).ok())
                            .or_else(|| {
                                params
                                    .get("min_clearance_mm")
                                    .and_then(|s| s.parse::<f64>().ok())
                            })
                            .filter(|v| v.is_finite() && *v >= 0.0);
                        let Some(min_clearance_mm) = min_clearance_mm else {
                            return Err(coded_err(
                                "E103_INVALID_VALUE",
                                "assert::no_interference",
                                "`min_clearance` must be a finite, nonnegative length".to_owned(),
                            ));
                        };
                        let result = (|| {
                            let a = self.interference_operand(kernel, "a")?;
                            let b = self.interference_operand(kernel, "b")?;
                            kernel.check_interference(&a, &b, min_clearance_mm)
                        })();
                        return match result {
                            Ok(None) => Ok(Vec::new()),
                            Ok(Some(violation)) => {
                                let message = match violation {
                                    InterferenceViolation::Overlap { volume_mm3 } => format!(
                                        "common volume {volume_mm3:.9e} mm³ exceeds 1e-4 mm³"
                                    ),
                                    InterferenceViolation::Clearance { distance_mm } => format!(
                                        "minimum distance {distance_mm:.9e} mm is below the required {min_clearance_mm:.9e} mm (tolerance: 1e-4 mm)"
                                    ),
                                };
                                Err(coded_err(
                                    "E506_INTERFERENCE_DETECTED",
                                    "assert::no_interference",
                                    message,
                                ))
                            }
                            Err(KernelError::UnsupportedMixedBoolean) => Err(coded_err(
                                "E507_UNSUPPORTED_MIXED_BOOLEAN",
                                "assert::no_interference",
                                "`assert::no_interference` requires exact solids; meshes and unliftable analytic features are unsupported".to_owned(),
                            )),
                            Err(KernelError::BackendUnavailable) => Err(coded_err(
                                "E504_UNSUPPORTED_IMPORT_FORMAT",
                                "assert::no_interference",
                                "`assert::no_interference` requires an OCCT-enabled build"
                                    .to_owned(),
                            )),
                            Err(err) => {
                                let diag = operation_diagnostic(err, Span::default());
                                Err(coded_err(
                                    &diag.code,
                                    "assert::no_interference",
                                    diag.message,
                                ))
                            }
                        };
                    }
                    "assert::physics" => {
                        return Ok(vec![NodeErrorDetail {
                            code: "W_PHYSICS_UNVERIFIED".to_owned(),
                            operation: Some("assert::physics".to_owned()),
                            native_status: None,
                            message: "`assert::physics` structural FEA is not yet implemented and was not executed; the physics constraints are unverified".to_owned(),
                        }]);
                    }
                    _ => {}
                }
            }

            // Metadata/contract assertions without a solid target (e.g. `__design_contract`)
            // and declarative L2/L3 design assertions are checked by `cad-runtime`'s verification
            // layer rather than legacy bounding-box ports.
            if target.is_none()
                && self.raw(&["solid", "bbox_min", "bbox_max", "condition"]).is_none()
            {
                let _ = kind;
                return Ok(Vec::new());
            }
            if let Some(target_id) = target {
                let target_snap = self
                    .cache
                    .snapshots
                    .get(&target_id)
                    .ok_or_else(|| format!("missing target node `{target_id}`"))?;
                let solid = target_snap
                    .solid
                    .as_ref()
                    .ok_or("missing effective upstream solid")?;
                let check = check_watertight(kernel, solid)?;
                if let cad_ir::DesignAssertionKind::VolumeBounds { min_mm3, max_mm3 } = kind {
                    if let Some(min) = min_mm3
                        && check.volume_mm3 + 1e-6 < min
                    {
                        return Err(format!(
                            "solid volume {:.3} mm^3 is below minimum {:.3} mm^3",
                            check.volume_mm3, min
                        )
                        .into());
                    }
                    if let Some(max) = max_mm3
                        && check.volume_mm3 - 1e-6 > max
                    {
                        return Err(format!(
                            "solid volume {:.3} mm^3 exceeds maximum {:.3} mm^3",
                            check.volume_mm3, max
                        )
                        .into());
                    }
                }
                return Ok(Vec::new());
            }
        }
        let condition = self.raw(&["condition"]);
        if let Some(condition) = condition {
            let met = match self.literal(condition) {
                IrValue::Bool(value) => *value,
                IrValue::NodeRef { node_id, .. }
                    if self.graph.nodes[&node_id.0].kind == NodeKind::AssertGate =>
                {
                    self.cache.snapshots[node_id].state == NodeState::Valid
                }
                _ => return Err("assert gate requires a boolean condition".into()),
            };
            if !met {
                return Err("assert gate condition is false".into());
            }
        }
        if self.raw(&["solid"]).is_none() && condition.is_some() {
            if self.raw(&["bbox_min", "bbox_max"]).is_none() {
                return Ok(Vec::new());
            }
            return Err("assert gate bounding-box bounds require a solid".into());
        }

        let check = check_watertight(kernel, self.primary_solid()?)?;
        let bounds = (check.bbox_mm.min, check.bbox_mm.max);
        for (port, direction, coordinates) in
            [("bbox_min", 1.0, bounds.0), ("bbox_max", -1.0, bounds.1)]
        {
            if let Some(limit) = self.input(&[port]) {
                let limit = vector(limit)?;
                if coordinates
                    .into_iter()
                    .zip(limit)
                    .any(|(actual, limit)| direction * (actual - limit) < -1e-6)
                {
                    return Err(format!("solid bounding box exceeds `{port}`").into());
                }
            }
        }
        Ok(Vec::new())
    }

    fn assertion_target_solid(&self, target: Option<&NodeId>) -> FeatureResult<&'a SolidShape> {
        if let Some(target_id) = target
            && let Some(snap) = self.cache.snapshots.get(target_id)
            && let Some(solid) = snap.solid.as_ref()
        {
            return Ok(solid);
        }
        self.primary_solid()
    }

    fn interference_operand(
        &self,
        kernel: &dyn GeometryKernel,
        port: &str,
    ) -> Result<Cow<'a, SolidShape>, KernelError> {
        let Some(IrValue::NodeRef { node_id, .. }) = self.raw(&[port]) else {
            return Err(KernelError::OperationFailed(format!(
                "`assert::no_interference` operand `{port}` did not evaluate to a Solid or Component"
            )));
        };
        if let Some(snap) = self.cache.snapshots.get(node_id)
            && let Some(solid) = snap.solid.as_ref()
        {
            return Ok(Cow::Borrowed(solid));
        }
        if let Some(CachedOutput::AssemblyComponent {
            component,
            local_solid,
            extref_payload,
            ..
        }) = self.cache.outputs.get(node_id)
        {
            let solid = local_solid
                .as_ref()
                .or_else(|| match extref_payload {
                    Some(ResolvedExtRefPayload::EagerSolid(s)) => Some(s.as_ref()),
                    _ => None,
                })
                .ok_or_else(|| {
                    KernelError::OperationFailed(format!(
                        "`assert::no_interference` operand `{port}` has no solid geometry"
                    ))
                })?;
            let origin = component.pose.translation_mm;
            let rotation = component.pose.rotation_deg;
            if !origin.iter().chain(rotation.iter()).all(|v| v.is_finite()) {
                return Err(KernelError::OperationFailed(format!(
                    "`assert::no_interference` operand `{port}` has a non-finite placement"
                )));
            }
            return kernel
                .transform(solid, placement(origin, rotation))
                .map(Cow::Owned);
        }
        Err(KernelError::OperationFailed(format!(
            "`assert::no_interference` operand `{port}` did not evaluate to a Solid or Component"
        )))
    }

    fn tag_faces(&self) -> Result<Evaluated, FeatureError> {
        let primary = self
            .primary()
            .ok_or("tag faces requires upstream geometry")?;
        let solid = self.primary_solid()?;
        let mut tagged = solid.clone();
        let mut topology = primary
            .topo_snapshot
            .clone()
            .ok_or("missing upstream topology")?;
        let mut tags = Vec::new();
        match self.input(&["tags", "tag"]) {
            Some(IrValue::List(values)) => {
                for value in values {
                    tags.push(string(self.literal(value))?);
                }
            }
            Some(value) => tags.push(string(value)?),
            None => return Err("missing face tag".into()),
        }
        if tags.is_empty() || tags.iter().any(|tag| tag.trim().is_empty()) {
            return Err("face tags must be nonempty".into());
        }
        let indices = if self.raw(&["faces"]).is_some() {
            self.face_indices("faces", solid)?
        } else {
            (0..solid.faces.len()).collect()
        };
        for index in indices {
            let face_tags = &mut tagged.faces[index].provenance_tags;
            for &tag in &tags {
                if !face_tags.iter().any(|existing| existing == tag) {
                    face_tags.push(tag.to_owned());
                }
            }
            topology.candidates[index]
                .provenance_tags
                .clone_from(face_tags);
        }
        Ok(Evaluated::Solid(tagged, Some(topology)))
    }

    fn face_indices(
        &self,
        port: &str,
        solid: &SolidShape,
    ) -> Result<BTreeSet<usize>, FeatureError> {
        let Some(IrValue::List(selectors)) = self.raw(&[port]) else {
            return Ok(BTreeSet::new());
        };
        let snapshot = self
            .primary()
            .and_then(|upstream| upstream.topo_snapshot.as_ref())
            .ok_or_else(|| FeatureError::from("missing upstream topology"))?;
        let mut exprs = Vec::with_capacity(selectors.len());
        for selector in selectors {
            let IrValue::TopoRefExpr(expr) = self.literal(selector) else {
                let message = format!("unavailable topology reference in `{port}`");
                return Err(FeatureError {
                    reason: message.clone(),
                    detail: Some(NodeErrorDetail {
                        code: "E202_EMPTY_SELECTOR".to_owned(),
                        operation: Some("tag::faces".to_owned()),
                        native_status: None,
                        message,
                    }),
                    sketch: None,
                    recovery_hint: None,
                });
            };
            if expr == "__foreign_solid_selector__" {
                let message =
                    "topology selector does not belong to its target solid".to_owned();
                return Err(FeatureError {
                    reason: message.clone(),
                    detail: Some(NodeErrorDetail {
                        code: "E202_EMPTY_SELECTOR".to_owned(),
                        operation: Some("tag::faces".to_owned()),
                        native_status: None,
                        message,
                    }),
                    sketch: None,
                    recovery_hint: None,
                });
            }
            exprs.push(expr.clone());
        }
        graph_mech_features::face_indices_for(snapshot, solid, &exprs, false)
    }

    fn failed_topo_refs(&self) -> Vec<String> {
        let mut refs = BTreeSet::new();
        for input in self.node.inputs.values() {
            visit_values(input, &mut |value| {
                if let IrValue::TopoRefExpr(reference) = self.literal(value) {
                    refs.insert(reference.clone());
                }
            });
        }
        refs.into_iter().collect()
    }

    fn raw(&self, ports: &[&str]) -> Option<&'a IrValue> {
        ports.iter().find_map(|port| self.node.inputs.get(*port))
    }

    fn literal(&self, value: &'a IrValue) -> &'a IrValue {
        if let IrValue::NodeRef { node_id, .. } = value {
            let node = &self.graph.nodes[&node_id.0];
            if let NodeKind::ParameterRef { param_id } = &node.kind
                && self.cache.snapshots[node_id].state == NodeState::Valid
            {
                return &self.graph.parameters[&param_id.0].value;
            }
        }
        value
    }

    fn input(&self, ports: &[&str]) -> Option<&'a IrValue> {
        self.raw(ports).map(|value| self.literal(value))
    }

    pub(super) fn length(&self, ports: &[&str]) -> FeatureResult<f64> {
        let value = self
            .input(ports)
            .ok_or_else(|| format!("missing `{}`", ports[0]))?;
        length(value).map_err(|error| format!("`{}`: {error}", ports[0]))
    }

    #[cfg(feature = "implicit")]
    fn resolution(&self) -> FeatureResult<[usize; 3]> {
        let Some(value) = self.input(&["resolution"]) else {
            return Ok([24; 3]);
        };
        let invalid =
            || "resolution must be a positive integer or three positive integers".to_owned();
        let resolution = match value {
            IrValue::List(values) => {
                let [x, y, z] = values.as_slice() else {
                    return Err(invalid());
                };
                [
                    whole_number(x).ok_or_else(invalid)? as usize,
                    whole_number(y).ok_or_else(invalid)? as usize,
                    whole_number(z).ok_or_else(invalid)? as usize,
                ]
            }
            value => [whole_number(value).ok_or_else(invalid)? as usize; 3],
        };
        if resolution.contains(&0) {
            return Err(invalid());
        }
        let cells = resolution.into_iter().try_fold(1_usize, usize::checked_mul);
        if !cells.is_some_and(|cells| cells <= MAX_LATTICE_CELLS) {
            return Err(format!(
                "implicit lattice resolution exceeds {MAX_LATTICE_CELLS} cells"
            ));
        }
        Ok(resolution)
    }

    fn string(&self, ports: &[&str]) -> FeatureResult<&'a str> {
        let value = self
            .input(ports)
            .ok_or_else(|| format!("missing `{}`", ports[0]))?;
        string(value).map_err(|error| format!("`{}`: {error}", ports[0]))
    }

    pub(super) fn node_span(&self) -> Span {
        let Some(raw) = self.node.metadata.get("dsl.ast_span") else {
            return Span::default();
        };
        let parts: Vec<usize> = raw.split(':').filter_map(|p| p.parse().ok()).collect();
        if let [start_line, start_col, end_line, end_col, start_byte, end_byte] = parts.as_slice() {
            Span {
                start_line: *start_line,
                start_col: *start_col,
                end_line: *end_line,
                end_col: *end_col,
                start_byte: *start_byte,
                end_byte: *end_byte,
            }
        } else {
            Span::default()
        }
    }

    pub(super) fn origin(&self) -> FeatureResult<[f64; 3]> {
        match self.input(&["origin"]) {
            None => Ok([0.0; 3]),
            Some(IrValue::Vec3(v)) => Ok(*v),
            Some(IrValue::List(values)) => match values.as_slice() {
                [x, y, z] => Ok([
                    length(self.literal(x))?,
                    length(self.literal(y))?,
                    length(self.literal(z))?,
                ]),
                _ => Err("expected three coordinates".into()),
            },
            Some(_) => Err("expected three coordinates".into()),
        }
    }

    fn upstream(&self, value: &IrValue) -> Option<&'a NodeSnapshot> {
        match value {
            IrValue::NodeRef { node_id, .. } => self.cache.get(node_id),
            _ => None,
        }
    }

    fn primary_value(&self) -> Option<&'a IrValue> {
        let list_head = |port| match self.raw(&[port]) {
            Some(IrValue::List(values)) => values.first(),
            _ => None,
        };
        let boolean_base = || self.raw(BOOLEAN_BASE_PORTS).or_else(|| list_head("solids"));
        match self.node.kind {
            NodeKind::BooleanUnion | NodeKind::ImplicitSmoothBlend => boolean_base()
                .or_else(|| list_head("tools"))
                .or_else(|| self.raw(BOOLEAN_TOOL_PORTS)),
            NodeKind::BooleanSubtract | NodeKind::BooleanIntersect => boolean_base(),
            NodeKind::TagFaces => self.raw(TAG_GEOMETRY_PORTS),
            NodeKind::AssertGate => self.raw(&["solid"]),
            NodeKind::ImplicitLatticeInfill => {
                self.raw(&["domain"]).or_else(|| self.raw(SOLID_PORTS))
            }
            _ => self.raw(SOLID_PORTS).or_else(|| self.raw(&["geometry"])),
        }
    }

    fn primary(&self) -> Option<&'a NodeSnapshot> {
        self.primary_value()
            .and_then(|value| self.upstream(value))
    }

    fn boolean_operand_values(&self) -> Vec<&'a IrValue> {
        let mut values = Vec::new();
        if let Some(base) = self.raw(BOOLEAN_BASE_PORTS) {
            values.push(base);
        }
        for port in ["solids", "tools"] {
            if let Some(IrValue::List(items)) = self.raw(&[port]) {
                values.extend(items);
            }
        }
        if let Some(tool) = self.raw(BOOLEAN_TOOL_PORTS) {
            values.push(tool);
        }
        values
    }

    /// Parents whose faces may survive. A subtract tool is omitted so the bore
    /// wall is created by this step instead of inheriting the drill.
    fn lineage_parents(&self) -> Vec<LineageParent> {
        match self.node.kind {
            NodeKind::BooleanUnion
            | NodeKind::BooleanIntersect
            | NodeKind::ImplicitSmoothBlend => self
                .boolean_operand_values()
                .into_iter()
                .filter_map(|value| self.lineage_parent(value))
                .collect(),
            NodeKind::BooleanSubtract
            | NodeKind::Fillet
            | NodeKind::Chamfer
            | NodeKind::Shell
            | NodeKind::Hole
            | NodeKind::PatternLinear
            | NodeKind::PatternPolar
            | NodeKind::Mirror
            | NodeKind::Draft
            | NodeKind::DirectFaceEdit
            | NodeKind::StdLibCall => self.primary_lineage_parent().into_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn primary_lineage_parent(&self) -> Option<LineageParent> {
        self.primary_value()
            .and_then(|value| self.lineage_parent(value))
    }

    fn lineage_parent(&self, value: &IrValue) -> Option<LineageParent> {
        let IrValue::NodeRef { node_id, .. } = value else {
            return None;
        };
        let snap = self.upstream(value)?;
        let solid = snap.solid.as_ref()?;
        let snapshot = snap
            .topo_snapshot
            .clone()
            .unwrap_or_else(|| snapshot_topology(node_id, solid));
        Some(LineageParent {
            snapshot,
            faces: face_samples(&solid.faces),
            edges: edge_samples(&solid.edges),
        })
    }

    fn propagate_output_topology(&self, solid: &SolidShape) -> TopoSnapshot {
        let parents = self.lineage_parents();
        let fresh_slots = if parents.is_empty() {
            FreshSlotStyle::KernelOrder
        } else {
            FreshSlotStyle::Geometric
        };
        propagate_lineage(
            &self.node.id.0,
            &parents,
            &face_samples(&solid.faces),
            &edge_samples(&solid.edges),
            &EvolutionOptions {
                tolerances: EvolutionTolerances::default(),
                fresh_slots,
            },
        )
    }

    fn primary_solid(&self) -> FeatureResult<&'a SolidShape> {
        self.primary()
            .and_then(|snapshot| snapshot.solid.as_ref())
            .ok_or_else(|| "missing effective upstream solid".into())
    }
}

#[allow(clippy::large_enum_variant)]
enum Evaluated {
    PassThrough,
    PassThroughWithWarnings(Vec<NodeErrorDetail>),
    Solid(SolidShape, Option<TopoSnapshot>),
    SolidWithWarnings(SolidShape, Option<TopoSnapshot>, Vec<NodeErrorDetail>),
    Track(CachedOutput),
}

#[cfg(any(feature = "implicit", feature = "subd"))]
fn whole_number(value: &IrValue) -> Option<u32> {
    match value {
        IrValue::Int(value) => u32::try_from(*value).ok(),
        IrValue::Scalar(value)
            if (0.0..=f64::from(u32::MAX)).contains(value) && value.fract() == 0.0 =>
        {
            Some(*value as u32)
        }
        _ => None,
    }
}

#[cfg(feature = "subd")]
fn quad_grid_surface_spec(
    mesh: &SubDMesh,
    faces: &[Vec<u32>],
    thickness_mm: f64,
) -> FeatureResult<QuadGridSurfaceSpec> {
    if faces.is_empty() {
        return Err("SubD mesh contains no faces".into());
    }
    if faces.iter().any(|face| face.len() != 4) {
        return Err("SubD-to-NURBS requires an all-quad mesh".into());
    }

    let mut edges = BTreeMap::<(u32, u32), Vec<(usize, usize)>>::new();
    for (face_index, face) in faces.iter().enumerate() {
        for edge_index in 0..4 {
            let a = face[edge_index];
            let b = face[(edge_index + 1) % 4];
            edges
                .entry((a.min(b), a.max(b)))
                .or_default()
                .push((face_index, edge_index));
        }
    }
    if edges.values().any(|incidences| incidences.len() > 2) {
        return Err("SubD mesh contains a non-manifold edge".into());
    }

    type Coord = (i32, i32);
    let mut face_coords = vec![None::<[Coord; 4]>; faces.len()];
    let mut vertex_coords = BTreeMap::<u32, Coord>::new();
    let mut pending = vec![0usize];
    face_coords[0] = Some([(0, 0), (1, 0), (1, 1), (0, 1)]);

    while let Some(face_index) = pending.pop() {
        let coords = face_coords[face_index].expect("assigned face coordinate");
        let face = &faces[face_index];
        for corner in 0..4 {
            let vertex = face[corner];
            if (vertex as usize) >= mesh.vertices.len() {
                return Err("SubD face references a vertex outside the mesh".into());
            }
            if let Some(previous) = vertex_coords.insert(vertex, coords[corner])
                && previous != coords[corner]
            {
                return Err("SubD mesh cannot be embedded in a regular quad grid".into());
            }
        }

        for edge_index in 0..4 {
            let a = face[edge_index];
            let b = face[(edge_index + 1) % 4];
            let Some(incidences) = edges.get(&(a.min(b), a.max(b))) else {
                continue;
            };
            let Some(&(neighbor_index, _neighbor_edge)) =
                incidences.iter().find(|&&(index, _)| index != face_index)
            else {
                continue;
            };
            if face_coords[neighbor_index].is_some() {
                continue;
            }

            let mut neighbor = [(0, 0); 4];
            let neighbor_face = &faces[neighbor_index];
            let Some(a_slot) = neighbor_face.iter().position(|vertex| *vertex == a) else {
                return Err("SubD edge connectivity is inconsistent".into());
            };
            let Some(b_slot) = neighbor_face.iter().position(|vertex| *vertex == b) else {
                return Err("SubD edge connectivity is inconsistent".into());
            };
            let source_a = coords[edge_index];
            let source_b = coords[(edge_index + 1) % 4];
            let delta = (source_b.0 - source_a.0, source_b.1 - source_a.1);
            let outward = (delta.1, -delta.0);
            let next = |slot: usize| (slot + 1) % 4;
            let previous = |slot: usize| (slot + 3) % 4;

            neighbor[a_slot] = source_a;
            neighbor[b_slot] = source_b;
            if next(a_slot) == b_slot {
                let offset = (-outward.0, -outward.1);
                neighbor[next(b_slot)] = (source_b.0 + offset.0, source_b.1 + offset.1);
                neighbor[previous(a_slot)] = (source_a.0 + offset.0, source_a.1 + offset.1);
            } else if next(b_slot) == a_slot {
                let offset = outward;
                neighbor[next(a_slot)] = (source_a.0 + offset.0, source_a.1 + offset.1);
                neighbor[previous(b_slot)] = (source_b.0 + offset.0, source_b.1 + offset.1);
            } else {
                return Err("SubD adjacent faces do not share a complete edge".into());
            }
            face_coords[neighbor_index] = Some(neighbor);
            pending.push(neighbor_index);
        }
    }

    if face_coords.iter().any(Option::is_none) {
        return Err("SubD quad faces are disconnected".into());
    }
    let mut grid_vertices = BTreeMap::<Coord, u32>::new();
    let mut grid_cells = BTreeSet::<Coord>::new();
    for (face, coords) in faces.iter().zip(face_coords) {
        let coords = coords.expect("checked above");
        let min_face_u = coords.iter().map(|coord| coord.0).min().unwrap_or(0);
        let max_face_u = coords.iter().map(|coord| coord.0).max().unwrap_or(0);
        let min_face_v = coords.iter().map(|coord| coord.1).min().unwrap_or(0);
        let max_face_v = coords.iter().map(|coord| coord.1).max().unwrap_or(0);
        if max_face_u - min_face_u != 1 || max_face_v - min_face_v != 1 {
            return Err("SubD face is not a regular unit quad".into());
        }
        if !grid_cells.insert((min_face_u, min_face_v)) {
            return Err("SubD faces overlap in the regular quad grid".into());
        }
        for corner in 0..4 {
            if let Some(previous) = grid_vertices.insert(coords[corner], face[corner])
                && previous != face[corner]
            {
                return Err("SubD faces overlap in the regular quad grid".into());
            }
        }
    }
    let min_u = grid_vertices.keys().map(|coord| coord.0).min().unwrap_or(0);
    let max_u = grid_vertices.keys().map(|coord| coord.0).max().unwrap_or(0);
    let min_v = grid_vertices.keys().map(|coord| coord.1).min().unwrap_or(0);
    let max_v = grid_vertices.keys().map(|coord| coord.1).max().unwrap_or(0);
    let nu = usize::try_from(max_u - min_u + 1).map_err(|_| "invalid grid width")?;
    let nv = usize::try_from(max_v - min_v + 1).map_err(|_| "invalid grid height")?;
    let expected_vertices = nu.checked_mul(nv).ok_or("grid dimensions overflow")?;
    if expected_vertices != grid_vertices.len() {
        return Err("SubD faces do not form a complete regular grid".into());
    }
    let expected_cells = (nu - 1)
        .checked_mul(nv - 1)
        .ok_or("grid cell count overflow")?;
    if expected_cells != grid_cells.len() {
        return Err("SubD faces do not form a complete regular grid".into());
    }
    let mut xyz_grid = Vec::with_capacity(expected_vertices);
    for u in min_u..=max_u {
        for v in min_v..=max_v {
            let vertex = grid_vertices
                .get(&(u, v))
                .ok_or("SubD faces do not form a complete regular grid")?;
            xyz_grid.push(mesh.vertices[*vertex as usize]);
        }
    }
    Ok(QuadGridSurfaceSpec {
        nu,
        nv,
        xyz_grid,
        thickness_mm,
    })
}

fn visit_values<'a>(value: &'a IrValue, visit: &mut impl FnMut(&'a IrValue)) {
    visit(value);
    match value {
        IrValue::List(values) => values.iter().for_each(|value| visit_values(value, visit)),
        IrValue::Map(values) => values.values().for_each(|value| visit_values(value, visit)),
        _ => {}
    }
}

fn visit_values_with_ext_refs<'a>(
    graph: &'a CanonicalGraph,
    value: &'a IrValue,
    visit: &mut impl FnMut(&'a IrValue),
) {
    let mut visited_ext = BTreeSet::new();
    fn walk<'a>(
        graph: &'a CanonicalGraph,
        value: &'a IrValue,
        visited_ext: &mut BTreeSet<&'a str>,
        visit: &mut impl FnMut(&'a IrValue),
    ) {
        visit(value);
        match value {
            IrValue::ExternalRef(ref_id) => {
                if visited_ext.insert(ref_id.0.as_str())
                    && let Some(def) = graph.external_refs.get(&ref_id.0)
                {
                    for ov in def.overrides.values() {
                        walk(graph, ov, visited_ext, visit);
                    }
                }
            }
            IrValue::List(values) => {
                for item in values {
                    walk(graph, item, visited_ext, visit);
                }
            }
            IrValue::Map(values) => {
                for item in values.values() {
                    walk(graph, item, visited_ext, visit);
                }
            }
            _ => {}
        }
    }
    walk(graph, value, &mut visited_ext, visit);
}

fn length(value: &IrValue) -> FeatureResult<f64> {
    match value {
        IrValue::Quantity(quantity) if quantity.ir_type() == IrType::Length => {
            Ok(quantity.canonical_value())
        }
        IrValue::Scalar(value) => Ok(*value),
        IrValue::Int(value) => Ok(*value as f64),
        _ => Err("expected a length".into()),
    }
}

fn vector(value: &IrValue) -> FeatureResult<[f64; 3]> {
    match value {
        IrValue::Vec3(value) => Ok(*value),
        IrValue::List(values) => match values.as_slice() {
            [x, y, z] => Ok([length(x)?, length(y)?, length(z)?]),
            _ => Err("expected three coordinates".into()),
        },
        _ => Err("expected three coordinates".into()),
    }
}

fn string(value: &IrValue) -> FeatureResult<&str> {
    match value {
        IrValue::String(value) => Ok(value),
        _ => Err("expected a string".into()),
    }
}

fn positive(value: f64, label: &str) -> FeatureResult<f64> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(format!("{label} must be finite and positive"))
    }
}

fn valid_bbox(bounds: BBox3) -> bool {
    bounds.min.into_iter().zip(bounds.max).all(|(min, max)| {
        min.is_finite() && max.is_finite() && (max - min).is_finite() && max > min
    })
}

fn validate_l1_report(solid: &SolidShape, check: &CheckReport) -> Result<(), String> {
    if solid.faces.is_empty() {
        return Err("solid has zero B-Rep faces".to_owned());
    }
    if !valid_bbox(check.bbox_mm) {
        return Err("solid has an invalid bounding box".to_owned());
    }
    if !check.watertight || check.open_edge_count > 0 {
        let detail = if check.issues.is_empty() {
            format!(
                "watertight={} open_edges={}",
                check.watertight, check.open_edge_count
            )
        } else {
            check.issues.join("; ")
        };
        return Err(format!("solid is not watertight: {detail}"));
    }
    Ok(())
}

fn check_watertight(kernel: &dyn GeometryKernel, solid: &SolidShape) -> FeatureResult<CheckReport> {
    if solid.faces.is_empty() {
        return Err("solid has zero B-Rep faces".into());
    }
    let check = kernel.check_solid(solid);
    validate_l1_report(solid, &check)?;
    Ok(check)
}

fn face_samples(faces: &[BRepFace]) -> Vec<TopologySample> {
    faces.iter().map(face_sample).collect()
}

fn edge_samples(edges: &[BRepEdge]) -> Vec<TopologySample> {
    edges.iter().map(edge_sample).collect()
}

fn face_sample(face: &BRepFace) -> TopologySample {
    TopologySample {
        fingerprint: face_fingerprint(face),
        provenance_tags: face.provenance_tags.clone(),
        inner_loop_count: u32::try_from(face.inner_loops.len()).unwrap_or(u32::MAX),
        outer_loop: face.outer_loop.clone(),
        secondary_loop: face.top_loop.clone().unwrap_or_default(),
    }
}

fn edge_sample(edge: &BRepEdge) -> TopologySample {
    TopologySample {
        fingerprint: edge_fingerprint(edge),
        provenance_tags: edge.provenance_tags.clone(),
        inner_loop_count: 0,
        outer_loop: edge
            .polyline
            .chunks_exact(3)
            .map(|point| [point[0], point[1], point[2]])
            .collect(),
        secondary_loop: Vec::new(),
    }
}

fn snapshot_topology(node: &NodeId, solid: &SolidShape) -> TopoSnapshot {
    propagate_lineage(
        &node.0,
        &[],
        &face_samples(&solid.faces),
        &edge_samples(&solid.edges),
        &EvolutionOptions::default(),
    )
}

fn face_fingerprint(face: &BRepFace) -> GeometricFingerprint {
    GeometricFingerprint {
        entity_kind: EntityKind::Face,
        surface_class: Some(match face.surface_type {
            SurfaceType::Plane => SurfaceClass::Plane,
            SurfaceType::Cylinder => SurfaceClass::Cylinder,
            SurfaceType::Cone => SurfaceClass::Cone,
            SurfaceType::Sphere => SurfaceClass::Sphere,
            SurfaceType::Torus => SurfaceClass::Torus,
            SurfaceType::Bspline => SurfaceClass::BSpline,
            SurfaceType::Mesh => SurfaceClass::Other,
        }),
        curve_class: None,
        centroid_mm: face.center_hint,
        normal_or_axis: Some(face.normal_hint),
        area_or_length_mm: face.area_mm2,
        radius_mm: face.radius_hint,
        min_curvature: None,
        max_curvature: None,
        adjacent_signature: Vec::new(),
    }
}

fn edge_fingerprint(edge: &BRepEdge) -> GeometricFingerprint {
    let points = edge.polyline.as_chunks::<3>().0;
    let point_count = points.len().max(1) as f64;
    let mut centroid = [0.0; 3];
    let mut length = 0.0;
    let mut previous: Option<&[f64; 3]> = None;
    for point in points {
        for (center, coordinate) in centroid.iter_mut().zip(point) {
            *center += coordinate / point_count;
        }
        if let Some(previous) = previous {
            length += previous
                .iter()
                .zip(point)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
        }
        previous = Some(point);
    }
    GeometricFingerprint {
        entity_kind: EntityKind::Edge,
        surface_class: None,
        curve_class: Some(match edge.curve_type {
            CurveType::Line => CurveClass::Line,
            CurveType::Circle | CurveType::Arc => CurveClass::Circle,
            CurveType::Ellipse => CurveClass::Ellipse,
            CurveType::Bspline => CurveClass::BSpline,
            CurveType::Polyline => CurveClass::Other,
        }),
        centroid_mm: centroid,
        normal_or_axis: None,
        area_or_length_mm: length,
        radius_mm: None,
        min_curvature: None,
        max_curvature: None,
        adjacent_signature: Vec::new(),
    }
}

fn resolve_import_asset_format(
    explicit_format: Option<&str>,
    source_name: Option<&str>,
    bytes: &[u8],
) -> FeatureResult<ImportFormat> {
    if let Some(raw) = explicit_format {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "step" | "stp" => Ok(ImportFormat::Step),
            "brep" => Ok(ImportFormat::Brep),
            "iges" | "igs" => Ok(ImportFormat::Iges),
            "stl" => Ok(ImportFormat::Stl),
            "3mf" => Ok(ImportFormat::ThreeMf),
            "obj" => Ok(ImportFormat::Obj),
            "gltf" | "glb" => Ok(ImportFormat::Gltf),
            other => Err(format!("unsupported import asset format `{other}`")),
        };
    }
    if let Some(name) = source_name
        && let Some(fmt) = ImportFormat::from_path(name)
    {
        return Ok(fmt);
    }
    if bytes.starts_with(b"ISO-10303-21;") {
        return Ok(ImportFormat::Step);
    }
    if bytes.starts_with(b"glTF") {
        return Ok(ImportFormat::Gltf);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok(ImportFormat::ThreeMf);
    }
    Err("cannot determine import asset format; specify `format` or `source_file`".into())
}

/// Convert a `GraphEvaluator` evaluation report into the standard `EvaluationOutput`
/// shared by `cad-cli` and the Tauri desktop host.
pub fn graph_report_to_evaluation_output(
    graph: &CanonicalGraph,
    report: GraphEvalReport,
    kernel: &dyn GeometryKernel,
    compile_time_ms: f64,
) -> EvaluationOutput {
    let mut diagnostics = Vec::new();
    for (node_id, nr) in &report.node_reports {
        if let NodeState::FailedWithRollback { reason, .. } = &nr.status {
            let code = nr
                .detail
                .as_ref()
                .map(|d| d.code.clone())
                .unwrap_or_else(|| "E207_INVALID_FEATURE".to_owned());
            let detail = nr.error.clone().unwrap_or_else(|| reason.clone());
            diagnostics.push(Diagnostic {
                code,
                severity: Severity::Error,
                message: format!("node `{}`: {detail}", node_id.0),
                span: Span::default(),
            });
        }
    }
    for warnings in report.warnings.values() {
        for warn in warnings {
            diagnostics.push(Diagnostic {
                code: warn.code.clone(),
                severity: Severity::Warning,
                message: warn.message.clone(),
                span: Span::default(),
            });
        }
    }

    let is_assembly = graph.outputs.iter().any(|id| {
        graph
            .nodes
            .get(&id.0)
            .is_some_and(|node| node.kind == NodeKind::AssemblyComponent)
    });

    if is_assembly {
        let mut asm_solid = SolidShape::empty(&graph.graph_id);
        let mut csg_items = Vec::with_capacity(graph.outputs.len());
        let mut component_solids = HashMap::new();
        let mut has_imported = false;
        let mut has_preview = false;
        let mut has_other_failure = false;
        let mut grouped = CheckReport {
            watertight: !graph.outputs.is_empty(),
            volume_mm3: 0.0,
            surface_area_mm2: 0.0,
            bbox_mm: BBox3::empty(),
            face_count: 0,
            edge_count: 0,
            open_edge_count: 0,
            issues: Vec::new(),
        };

        for (idx, out_id) in graph.outputs.iter().enumerate() {
            let Some(mut local_solid) = report.local_component_solids.get(out_id).cloned() else {
                has_other_failure = true;
                grouped.watertight = false;
                continue;
            };
            let part_name = report
                .component_part_names
                .get(out_id)
                .filter(|name| !name.is_empty())
                .cloned()
                .or_else(|| {
                    (!local_solid.part_name.is_empty()).then(|| local_solid.part_name.clone())
                })
                .unwrap_or_else(|| out_id.0.clone());
            let (origin, rotation_deg) = report
                .component_poses
                .get(out_id)
                .copied()
                .unwrap_or(([0.0; 3], [0.0; 3]));
            local_solid.part_name = part_name.clone();
            let cid = format!("comp_{idx}_{part_name}");
            let mat = placement(origin, rotation_deg);
            let mut placed = match kernel.transform(&local_solid, mat) {
                Ok(placed) => placed,
                Err(err) => {
                    diagnostics.push(operation_diagnostic(err, Span::default()));
                    has_other_failure = true;
                    grouped.watertight = false;
                    continue;
                }
            };
            let preview = is_imported_preview(&local_solid);
            has_imported |= !matches!(local_solid.payload, KernelPayload::Analytic);
            has_preview |= preview;
            component_solids.insert(cid.clone(), local_solid);
            for f in &mut placed.faces {
                f.component_id = Some(cid.clone());
            }
            for e in &mut placed.edges {
                e.component_id = Some(cid.clone());
            }
            let mut chk = kernel.check_solid(&placed);
            if matches!(placed.payload, KernelPayload::TriMesh) {
                let mut bounds = BBox3::empty();
                for point in placed
                    .faces
                    .iter()
                    .flat_map(|f| f.vertices.as_chunks::<3>().0)
                {
                    bounds.expand_point(*point);
                }
                chk.bbox_mm = bounds;
            }
            grouped.watertight &= chk.watertight;
            grouped.volume_mm3 += chk.volume_mm3;
            grouped.surface_area_mm2 += chk.surface_area_mm2;
            grouped.bbox_mm = grouped.bbox_mm.union(chk.bbox_mm);
            grouped.face_count += chk.face_count;
            grouped.edge_count += chk.edge_count;
            grouped.open_edge_count += chk.open_edge_count;
            grouped
                .issues
                .extend(chk.issues.iter().map(|issue| format!("{cid}: {issue}")));
            if !chk.watertight && !preview {
                has_other_failure = true;
                diagnostics.push(Diagnostic {
                    code: "E301_NON_WATERTIGHT".to_owned(),
                    severity: Severity::Error,
                    message: format!(
                        "component `{cid}` failed watertight check: {:?}",
                        chk.issues
                    ),
                    span: Span::default(),
                });
            }
            asm_solid.components.push(ComponentMeta {
                component_id: cid,
                part_name,
                origin_mm: origin,
                rotation_deg,
                volume_mm3: chk.volume_mm3,
                bbox_mm: chk.bbox_mm,
            });
            for f in &mut placed.faces {
                f.face_id = format!("comp_{idx}_{}", f.face_id);
            }
            for e in &mut placed.edges {
                e.edge_id = format!("comp_{idx}_{}", e.edge_id);
            }
            csg_items.push(placed.csg);
            asm_solid.faces.extend(placed.faces);
            asm_solid.edges.extend(placed.edges);
            asm_solid.pmi.extend(placed.pmi);
            asm_solid.exact_brep &= placed.exact_brep;
        }
        asm_solid.csg = CsgNode::Union(csg_items);
        if has_imported {
            asm_solid.exact_brep = false;
        }
        let (check_report, preview_only) = if has_imported {
            (grouped, has_preview && !has_other_failure)
        } else {
            let chk = kernel.check_solid(&asm_solid);
            (chk, false)
        };
        if !check_report.watertight && !preview_only {
            diagnostics.push(Diagnostic {
                code: "E301_NON_WATERTIGHT".to_owned(),
                severity: Severity::Error,
                message: format!(
                    "solid `{}` failed watertight check: {:?}",
                    graph.graph_id, check_report.issues
                ),
                span: Span::default(),
            });
        }
        let status_ok = !diagnostics.iter().any(|d| d.severity == Severity::Error);
        let scene_mesh = build_scene_mesh(&asm_solid, Vec::new());
        let kernel_backend = if component_solids
            .values()
            .any(|s| !matches!(s.payload, KernelPayload::Analytic))
        {
            "mixed-assembly"
        } else {
            kernel.backend_name()
        };
        return EvaluationOutput {
            status_ok,
            part_name: graph.graph_id.clone(),
            is_assembly: true,
            diagnostics,
            check_report,
            solid: asm_solid,
            component_solids,
            scene_mesh,
            compile_time_ms,
            kernel_backend: kernel_backend.to_owned(),
        };
    }

    let Some((out_id, mut solid)) = graph
        .outputs
        .first()
        .and_then(|id| report.output_solids.get(id).map(|s| (id, s.clone())))
    else {
        if !diagnostics.iter().any(|d| d.severity == Severity::Error) {
            diagnostics.push(Diagnostic {
                code: "E207_INVALID_FEATURE".to_owned(),
                severity: Severity::Error,
                message: format!(
                    "graph `{}` evaluation produced no output solid",
                    graph.graph_id
                ),
                span: Span::default(),
            });
        }
        let empty = SolidShape::empty(&graph.graph_id);
        let chk = kernel.check_solid(&empty);
        let scene_mesh = build_scene_mesh(&empty, Vec::new());
        return EvaluationOutput {
            status_ok: false,
            part_name: graph.graph_id.clone(),
            is_assembly: false,
            diagnostics,
            check_report: CheckReport {
                watertight: false,
                volume_mm3: 0.0,
                surface_area_mm2: 0.0,
                bbox_mm: BBox3::default(),
                face_count: 0,
                edge_count: 0,
                open_edge_count: 0,
                issues: chk.issues,
            },
            solid: empty,
            component_solids: HashMap::new(),
            scene_mesh,
            compile_time_ms,
            kernel_backend: kernel.backend_name().to_owned(),
        };
    };

    solid.part_name = graph.graph_id.clone();
    let check_report = report
        .output_checks
        .get(out_id)
        .cloned()
        .unwrap_or_else(|| kernel.check_solid(&solid));
    let preview_only = !check_report.watertight && is_imported_preview(&solid);
    if !check_report.watertight
        && !preview_only
        && !diagnostics.iter().any(|d| d.code == "E301_NON_WATERTIGHT")
    {
        diagnostics.push(Diagnostic {
            code: "E301_NON_WATERTIGHT".to_owned(),
            severity: Severity::Error,
            message: format!(
                "solid `{}` failed watertight check: {:?}",
                graph.graph_id, check_report.issues
            ),
            span: Span::default(),
        });
    }
    let status_ok = !diagnostics.iter().any(|d| d.severity == Severity::Error);
    let mut scene_mesh = build_scene_mesh(&solid, Vec::new());
    if status_ok && check_report.watertight {
        scene_mesh.hole_offsets = hole_offset_handles(kernel, &solid, Span::default());
    }
    let kernel_backend = match &solid.payload {
        KernelPayload::Occt(_) => "occt-ffi",
        KernelPayload::TriMesh => "triangle-mesh",
        KernelPayload::Analytic => kernel.backend_name(),
    };
    EvaluationOutput {
        status_ok,
        part_name: graph.graph_id.clone(),
        is_assembly: false,
        diagnostics,
        check_report,
        solid,
        component_solids: HashMap::new(),
        scene_mesh,
        compile_time_ms,
        kernel_backend: kernel_backend.to_owned(),
    }
}

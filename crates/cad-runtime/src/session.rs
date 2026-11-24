//! One open `.aicad` document and the host that creates it.
//!
//! Every mutating call writes through `SqliteContainer` before it replaces the
//! in-memory graph. Undo moves the SQLite oplog cursor backwards. Redo patches
//! live only in this process: rollback deletes the forward oplog rows, so a
//! reopened file restores the cursor and the remaining undo history, not the
//! redo stack.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cad_api::{
    CanonicalGraph, ChangeSummary, DesignContract, GraphPatch, NodeId, NodeRecoveryHint,
    SemanticCommand, apply_commands, inspect_document_summary,
};
use cad_container::{
    AicadProject, BranchInfo, BranchPurpose, BranchStatus, ContainerManifest, KernelFingerprint,
    KclProjectionPolicy, OplogActor, SqliteContainer,
};
use cad_evaluator::{GraphEvaluator, MerkleEvalCache, SolveStatus};
use cad_mesh_io::Mesh;
use cad_ir::{
    DesignAssertionKind, DimInput, FeatureSpec, IrValue, KernelIdentity, NodeKind, NodeState,
    TriVerdict, VerificationCheck, VerificationLayer, VerificationReport, resolve_formula_values,
    validate_graph,
};
use cad_kernel_occt::{
    BBox3, BRepEdge, BRepFace, ComponentMeta, CsgNode, CurveType, GeometryKernel, IBRepKernel,
    KernelError, KernelPayload, OcctImportFormat, OcctImportOptions, SolidShape, SurfaceType,
    default_brep_kernel, import_occt_bytes,
};
use cad_topo::{
    CurveClass, EntityKind, EvolutionOptions, GeometricFingerprint, SurfaceClass, TopoSnapshot,
    TopologySample, propagate_lineage,
};

use crate::error::RuntimeError;
use crate::sandbox::{canonicalize_workspace, resolve_in_workspace};
use crate::types::{
    Actor, BBoxDelta, BranchDiff, BranchTarget, CancelToken, CommitIfPassOutcome, ExecOutcome,
    ExportArtifactReport, HistoryDirection, HistoryOutcome, MetricDelta, RuntimeMode,
    SessionContext, SolidDigest, VerificationProfile, ViewportContext, ensure_running,
};

static COMMIT_IF_PASS_STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

fn default_kernel_and_identity() -> (Arc<dyn GeometryKernel + Send + Sync>, KernelIdentity) {
    let brep = default_brep_kernel();
    let identity = identity_from_brep_kernel(brep.as_ref());
    let kernel: Arc<dyn IBRepKernel> = Arc::from(brep);
    let kernel: Arc<dyn GeometryKernel + Send + Sync> = kernel;
    (kernel, identity)
}

fn identity_from_brep_kernel(kernel: &dyn IBRepKernel) -> KernelIdentity {
    let metadata = kernel.metadata();
    KernelIdentity::new(
        kernel.backend_name(),
        metadata.version,
        kernel.supports_authoritative_delivery(),
    )
}

fn identity_from_geometry_kernel(kernel: &dyn GeometryKernel) -> KernelIdentity {
    let backend = kernel.backend_name();
    KernelIdentity::new(
        backend,
        env!("CARGO_PKG_VERSION"),
        is_authoritative_backend(backend),
    )
}

fn is_authoritative_backend(backend_name: &str) -> bool {
    matches!(
        backend_name.to_ascii_lowercase().as_str(),
        "occt" | "occt-ffi"
    )
}

fn fingerprint_from_identity(identity: &KernelIdentity) -> KernelFingerprint {
    KernelFingerprint::new(&identity.provider, &identity.version)
}

struct RedoEntry {
    patch: GraphPatch,
    actor: OplogActor,
}

struct WriteTarget {
    branch: String,
    /// Live-attached external agents fork `agent/<client>/<session_id>` from `main`
    /// on the first write. The branch is created only after the revision check.
    fork_from_main: bool,
}

/// Host handle. Desktop and headless processes share the session type below.
pub struct DocumentRuntime {
    workspace_root: PathBuf,
    mode: RuntimeMode,
    ipc_endpoint: String,
    kernel: Arc<dyn GeometryKernel + Send + Sync>,
    kernel_identity: KernelIdentity,
}

impl DocumentRuntime {
    /// CLI, CI, or a git worktree. No discovery file is written.
    pub fn headless(workspace_root: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        Self::new(workspace_root.as_ref(), RuntimeMode::Headless)
    }

    /// Desktop host. The endpoint name is recorded; the pipe or socket is bound later.
    pub fn live_attached(workspace_root: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        Self::new(workspace_root.as_ref(), RuntimeMode::LiveAttached)
    }

    fn new(workspace_root: &Path, mode: RuntimeMode) -> Result<Self, RuntimeError> {
        let workspace_root = canonicalize_workspace(workspace_root)?;
        let ipc_endpoint = crate::discovery::default_ipc_endpoint(&workspace_root);
        let (kernel, kernel_identity) = default_kernel_and_identity();
        Ok(Self {
            workspace_root,
            mode,
            ipc_endpoint,
            kernel,
            kernel_identity,
        })
    }

    /// Inject a caller-supplied geometry kernel for dependency injection or testing.
    pub fn with_kernel(mut self, kernel: Arc<dyn GeometryKernel + Send + Sync>) -> Self {
        self.kernel_identity = identity_from_geometry_kernel(kernel.as_ref());
        self.kernel = kernel;
        self
    }

    pub fn mode(&self) -> RuntimeMode {
        self.mode
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn ipc_endpoint(&self) -> &str {
        &self.ipc_endpoint
    }

    pub fn kernel(&self) -> &Arc<dyn GeometryKernel + Send + Sync> {
        &self.kernel
    }

    pub fn kernel_identity(&self) -> KernelIdentity {
        self.kernel_identity.clone()
    }

    pub fn kernel_fingerprint(&self) -> KernelFingerprint {
        fingerprint_from_identity(&self.kernel_identity)
    }

    pub fn create_session(
        &self,
        path: impl AsRef<Path>,
        project_name: impl Into<String>,
    ) -> Result<DocumentSession, RuntimeError> {
        let mut session = DocumentSession::create_with_kernel_and_identity(
            path,
            &self.workspace_root,
            project_name,
            Arc::clone(&self.kernel),
            self.kernel_identity.clone(),
        )?;
        session.mode = self.mode;
        session.ipc_endpoint = self.ipc_endpoint.clone();
        Ok(session)
    }

    pub fn open_session(&self, path: impl AsRef<Path>) -> Result<DocumentSession, RuntimeError> {
        let mut session = DocumentSession::open_with_kernel_and_identity(
            path,
            &self.workspace_root,
            Arc::clone(&self.kernel),
            self.kernel_identity.clone(),
        )?;
        session.mode = self.mode;
        session.ipc_endpoint = self.ipc_endpoint.clone();
        Ok(session)
    }
}

/// Stateful document. The SQLite file is the source of truth after each commit.
pub struct DocumentSession {
    container: SqliteContainer,
    path: PathBuf,
    workspace_root: PathBuf,
    project_name: String,
    active_branch: String,
    revision: u64,
    graph: CanonicalGraph,
    kernel: Arc<dyn GeometryKernel + Send + Sync>,
    kernel_identity: KernelIdentity,
    merkle_cache: MerkleEvalCache,
    last_solids: SolidDigest,
    output_solids: BTreeMap<NodeId, SolidShape>,
    topo_cache: TopoSnapshot,
    eval_facts: EvalFacts,
    actor: Actor,
    viewport: ViewportContext,
    cancel: CancelToken,
    mode: RuntimeMode,
    ipc_endpoint: String,
    redo: BTreeMap<String, Vec<RedoEntry>>,
}

impl DocumentSession {
    /// Create a new `.aicad` file. An existing file at `path` is replaced by the container.
    pub fn create(
        path: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
        project_name: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let (kernel, kernel_identity) = default_kernel_and_identity();
        Self::create_with_kernel_and_identity(
            path,
            workspace_root,
            project_name,
            kernel,
            kernel_identity,
        )
    }

    /// Create a new `.aicad` file using an injected geometry kernel.
    pub fn create_with_kernel(
        path: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
        project_name: impl Into<String>,
        kernel: Arc<dyn GeometryKernel + Send + Sync>,
    ) -> Result<Self, RuntimeError> {
        let kernel_identity = identity_from_geometry_kernel(kernel.as_ref());
        Self::create_with_kernel_and_identity(
            path,
            workspace_root,
            project_name,
            kernel,
            kernel_identity,
        )
    }

    fn create_with_kernel_and_identity(
        path: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
        project_name: impl Into<String>,
        kernel: Arc<dyn GeometryKernel + Send + Sync>,
        kernel_identity: KernelIdentity,
    ) -> Result<Self, RuntimeError> {
        let project_name = project_name.into();
        if project_name.trim().is_empty() {
            return Err(RuntimeError::Invalid("project name is empty".into()));
        }
        let workspace_root = canonicalize_workspace(workspace_root.as_ref())?;
        let path = resolve_in_workspace(&workspace_root, path.as_ref())?;
        let graph = CanonicalGraph::new(&project_name);
        let mut manifest = ContainerManifest::new(
            &project_name,
            fingerprint_from_identity(&kernel_identity),
        );
        manifest.kcl_projection_policy = KclProjectionPolicy::OnDemandDynamic;
        manifest.kcl_view = None;
        let project = AicadProject::new(manifest, graph);
        let container = SqliteContainer::create(&path, &project)?;
        Self::from_container(
            container,
            path,
            workspace_root,
            RuntimeMode::Headless,
            kernel,
            kernel_identity,
        )
    }

    /// Open an existing `.aicad` file inside `workspace_root`.
    pub fn open(
        path: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Self, RuntimeError> {
        let (kernel, kernel_identity) = default_kernel_and_identity();
        Self::open_with_kernel_and_identity(path, workspace_root, kernel, kernel_identity)
    }

    /// Open an existing `.aicad` file inside `workspace_root` using an injected geometry kernel.
    pub fn open_with_kernel(
        path: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
        kernel: Arc<dyn GeometryKernel + Send + Sync>,
    ) -> Result<Self, RuntimeError> {
        let kernel_identity = identity_from_geometry_kernel(kernel.as_ref());
        Self::open_with_kernel_and_identity(path, workspace_root, kernel, kernel_identity)
    }

    fn open_with_kernel_and_identity(
        path: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
        kernel: Arc<dyn GeometryKernel + Send + Sync>,
        kernel_identity: KernelIdentity,
    ) -> Result<Self, RuntimeError> {
        let workspace_root = canonicalize_workspace(workspace_root.as_ref())?;
        let path = resolve_in_workspace(&workspace_root, path.as_ref())?;
        let container = SqliteContainer::open(&path)?;
        Self::from_container(
            container,
            path,
            workspace_root,
            RuntimeMode::Headless,
            kernel,
            kernel_identity,
        )
    }

    fn from_container(
        container: SqliteContainer,
        path: PathBuf,
        workspace_root: PathBuf,
        mode: RuntimeMode,
        kernel: Arc<dyn GeometryKernel + Send + Sync>,
        kernel_identity: KernelIdentity,
    ) -> Result<Self, RuntimeError> {
        let project = container.load_project()?;
        let active_branch = container.active_branch()?;
        let revision = head_of(&container, &active_branch)?;
        let graph = project.graph;
        let mut merkle_cache = MerkleEvalCache::new();
        let fingerprint = fingerprint_from_identity(&kernel_identity);
        let product = evaluate_graph_with_cache(
            kernel.as_ref(),
            &fingerprint,
            &mut merkle_cache,
            &path,
            &workspace_root,
            &graph,
        )?;
        let ipc_endpoint = crate::discovery::default_ipc_endpoint(&workspace_root);
        Ok(Self {
            container,
            path,
            workspace_root,
            project_name: project.manifest.project_name,
            active_branch,
            revision,
            graph,
            kernel,
            kernel_identity,
            merkle_cache,
            last_solids: product.digest,
            output_solids: product.solids,
            topo_cache: product.snapshot,
            eval_facts: product.facts,
            actor: Actor::HumanUi,
            viewport: ViewportContext::default(),
            cancel: CancelToken::new(),
            mode,
            ipc_endpoint,
            redo: BTreeMap::new(),
        })
    }

    /// Confirm the open branch still matches the SQLite head.
    ///
    /// Edits are already committed. This does not rewrite the file, because a
    /// full project rewrite would drop the oplog.
    pub fn save(&self) -> Result<(), RuntimeError> {
        let disk = self.container.load_active_graph()?;
        let head = head_of(&self.container, &self.active_branch)?;
        if disk != self.graph || head != self.revision {
            return Err(RuntimeError::PersistMismatch);
        }
        Ok(())
    }

    /// Write the live KCL view to a path inside the workspace.
    pub fn export_kcl(&self, dest: impl AsRef<Path>) -> Result<(), RuntimeError> {
        let path = resolve_in_workspace(&self.workspace_root, dest.as_ref())?;
        crate::write_file_atomic(&path, self.emit_kcl_view()?.as_bytes())?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn active_branch(&self) -> &str {
        &self.active_branch
    }

    pub fn graph(&self) -> &CanonicalGraph {
        &self.graph
    }

    pub fn solids(&self) -> &SolidDigest {
        &self.last_solids
    }

    pub fn kernel(&self) -> &Arc<dyn GeometryKernel + Send + Sync> {
        &self.kernel
    }

    pub fn kernel_identity(&self) -> KernelIdentity {
        self.kernel_identity.clone()
    }

    pub fn kernel_fingerprint(&self) -> KernelFingerprint {
        fingerprint_from_identity(&self.kernel_identity)
    }

    /// Name reported by the session geometry kernel (`occt` or `rust-brep-mvp`).
    pub fn kernel_backend_name(&self) -> &str {
        self.kernel.backend_name()
    }

    /// True when the session kernel is an authoritative OCCT backend.
    pub fn is_authoritative_occt(&self) -> bool {
        self.kernel_identity.authoritative
    }

    /// Topology candidates for the solids of the checked-out graph.
    pub fn topo_snapshot(&self) -> &TopoSnapshot {
        &self.topo_cache
    }

    /// Machine-readable recovery hints produced during the latest evaluation.
    pub fn recovery_hints(&self) -> &[NodeRecoveryHint] {
        &self.eval_facts.recovery_hints
    }

    /// Write a STEP, 3MF, DXF, or SVG file inside the workspace.
    ///
    /// `part_label` is a user-facing part name. It resolves to the finished
    /// solid, including when the name belongs to an upstream feature that a
    /// later feature consumed. `node_id` selects that node even when it is an
    /// intermediate feature.
    pub fn export_artifact(
        &self,
        format: &str,
        target_path: &Path,
        part_label: Option<&str>,
    ) -> Result<ExportArtifactReport, RuntimeError> {
        self.export_artifact_selection(format, target_path, part_label, None)
    }

    /// Same as [`Self::export_artifact`], plus an explicit node selection.
    pub fn export_artifact_selection(
        &self,
        format: &str,
        target_path: &Path,
        part_label: Option<&str>,
        node_id: Option<&str>,
    ) -> Result<ExportArtifactReport, RuntimeError> {
        export_artifact(self, format, target_path, part_label, node_id)
    }

    /// Run the fast or release verification profile. This does not mutate the document.
    pub fn verify(&self, profile: VerificationProfile) -> VerificationReport {
        verify_view(
            &VerifyView {
                graph: &self.graph,
                output_solids: &self.output_solids,
                eval_facts: &self.eval_facts,
                kernel: self.kernel.as_ref(),
                kernel_identity: &self.kernel_identity,
            },
            profile,
        )
    }

    /// Run the 3D matrix-free H8 voxel FEA structural gate against the session's
    /// current evaluated terminal solids using an explicit structural contract.
    ///
    /// Returns the `L3_FEA` `VerificationCheck`, a lightweight summary payload,
    /// and the grid-ordered per-voxel Von Mises stress field (`MPa`).
    pub fn evaluate_structural_fea(
        &self,
        contract: &crate::StructuralFeaContract,
    ) -> crate::L3FeaEvaluation {
        let terminal =
            select_solids(&self.graph, &self.output_solids, None, None).unwrap_or_default();
        let (triangles, _, _) = crate::l3_dfm::build_triangle_scene(&terminal);
        crate::l3_fea::evaluate_l3_fea_triangles(&triangles, contract)
    }

    /// Run verification on a named branch without mutating `main`, `active_branch`, or redo history.
    pub fn verify_branch(
        &mut self,
        branch_name: &str,
        profile: VerificationProfile,
    ) -> Result<VerificationReport, RuntimeError> {
        self.require_branch(branch_name)?;
        if branch_name == self.active_branch
            && head_of(&self.container, branch_name)? == self.revision
        {
            return Ok(self.verify(profile));
        }
        let graph = self.load_graph_for(branch_name)?;
        let product = self.evaluate_graph(&graph)?;
        Ok(verify_view(
            &VerifyView {
                graph: &graph,
                output_solids: &product.solids,
                eval_facts: &product.facts,
                kernel: self.kernel.as_ref(),
                kernel_identity: &self.kernel_identity,
            },
            profile,
        ))
    }

    pub fn mode(&self) -> RuntimeMode {
        self.mode
    }

    pub fn ipc_endpoint(&self) -> &str {
        &self.ipc_endpoint
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    pub fn set_actor(&mut self, actor: Actor) {
        self.actor = actor;
    }

    pub fn viewport(&self) -> &ViewportContext {
        &self.viewport
    }

    pub fn set_viewport(&mut self, viewport: ViewportContext) {
        self.viewport = viewport;
    }

    pub fn context(&self) -> SessionContext {
        SessionContext {
            revision: self.revision,
            active_branch: self.active_branch.clone(),
            project_name: self.project_name.clone(),
            actor: self.actor.clone(),
            mode: self.mode,
            viewport: self.viewport.clone(),
        }
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    pub fn request_cancel(&self) {
        self.cancel.cancel();
    }

    pub fn reset_cancel(&self) {
        self.cancel.reset();
    }

    /// Evaluate the open graph again. The Merkle cache is preserved across runs.
    pub fn re_evaluate(&mut self) -> Result<SolidDigest, RuntimeError> {
        let graph = self.graph.clone();
        let product = self.evaluate_graph(&graph)?;
        Ok(self.adopt(product))
    }

    pub fn exec_semantic_cmds(
        &mut self,
        expected_revision: Option<u64>,
        actor: Actor,
        target: BranchTarget,
        commands: Vec<SemanticCommand>,
    ) -> Result<ExecOutcome, RuntimeError> {
        self.exec_semantic_cmds_cancellable(expected_revision, actor, target, commands, None)
    }

    pub fn exec_semantic_cmds_cancellable(
        &mut self,
        expected_revision: Option<u64>,
        actor: Actor,
        target: BranchTarget,
        commands: Vec<SemanticCommand>,
        extra: Option<&CancelToken>,
    ) -> Result<ExecOutcome, RuntimeError> {
        ensure_running(&self.cancel, extra)?;
        let write = self.prepare_target(&actor, &target)?;
        let check_branch = if write.fork_from_main {
            "main"
        } else {
            write.branch.as_str()
        };
        self.check_revision(expected_revision, check_branch)?;
        let seen = head_of(&self.container, check_branch)?;
        let base_graph = self.load_graph_for(check_branch)?;

        if commands.is_empty() {
            return self.noop_outcome(&write.branch, &base_graph);
        }

        let applied = apply_commands(&base_graph, &commands)?;
        if applied.patch.ops.is_empty() {
            return self.noop_outcome(&write.branch, &base_graph);
        }
        ensure_running(&self.cancel, extra)?;
        // Fail closed before the SQLite commit when the graph itself cannot be evaluated.
        let preflight = self.evaluate_graph(&applied.graph)?;
        ensure_running(&self.cancel, extra)?;

        let now = head_of(&self.container, check_branch)?;
        if now != seen {
            return Err(self.conflict(seen, now, check_branch)?);
        }
        if write.fork_from_main {
            self.container.create_branch_with_purpose(
                &write.branch,
                Some("main"),
                BranchPurpose::AiCandidate,
            )?;
        }

        let entry = self.container.apply_patch_with_actor(
            &applied.patch,
            Some(&write.branch),
            to_oplog(&actor),
        )?;
        self.redo.remove(&write.branch);
        if write.branch == self.active_branch {
            self.actor = actor;
        }
        let persisted = self.container.load_branch_graph(&write.branch)?;
        let revision = seq_to_rev(entry.seq);
        let (solids, mut recovery_hints) =
            self.remember_branch(&write.branch, persisted, revision)?;
        if recovery_hints.is_empty() && !preflight.facts.recovery_hints.is_empty() {
            recovery_hints = preflight.facts.recovery_hints;
            if write.branch == self.active_branch {
                self.eval_facts.recovery_hints = recovery_hints.clone();
            }
        }
        Ok(ExecOutcome {
            revision,
            branch: write.branch,
            summary: applied.summary,
            created_nodes: applied.created_nodes,
            modified_nodes: applied.modified_nodes,
            deleted_nodes: applied.deleted_nodes,
            solids,
            recovery_hints,
        })
    }

    /// Stage a semantic command batch on an isolated preview branch, run Fast L0-L2
    /// verification, and fast-forward `main` only when all Fast layers pass.
    ///
    /// When execution or verification fails, the staging branch is discarded without
    /// modifying `main` or its redo stack.
    pub fn exec_semantic_cmds_commit_if_pass(
        &mut self,
        expected_revision: Option<u64>,
        actor: Actor,
        commands: Vec<SemanticCommand>,
    ) -> Result<CommitIfPassOutcome, RuntimeError> {
        self.exec_semantic_cmds_commit_if_pass_cancellable(expected_revision, actor, commands, None)
    }

    /// Cancellable variant of [`Self::exec_semantic_cmds_commit_if_pass`].
    pub fn exec_semantic_cmds_commit_if_pass_cancellable(
        &mut self,
        expected_revision: Option<u64>,
        actor: Actor,
        commands: Vec<SemanticCommand>,
        extra: Option<&CancelToken>,
    ) -> Result<CommitIfPassOutcome, RuntimeError> {
        ensure_running(&self.cancel, extra)?;
        if self.mode != RuntimeMode::Headless {
            return Err(RuntimeError::Invalid(
                "commit_if_pass is only available on a headless session".into(),
            ));
        }
        self.check_revision(expected_revision, "main")?;
        let main_rev = head_of(&self.container, "main")?;
        let staging = self.allocate_staging_branch()?;
        self.branch_preview(&staging)?;
        let staged = match self.exec_semantic_cmds_cancellable(
            Some(main_rev),
            actor.clone(),
            BranchTarget::Named(staging.clone()),
            commands,
            extra,
        ) {
            Ok(outcome) => outcome,
            Err(err) => {
                let _ = self.discard_preview(&staging);
                return Err(err);
            }
        };
        let report = match self.verify_branch(&staging, VerificationProfile::Fast) {
            Ok(report) => report,
            Err(err) => {
                let _ = self.discard_preview(&staging);
                return Err(err);
            }
        };
        if fast_verification_passes(&report) {
            let committed = match self.commit_preview_inner(&staging, Some(actor), extra) {
                Ok(outcome) => outcome,
                Err(err) => {
                    let _ = self.discard_preview(&staging);
                    return Err(err);
                }
            };
            let recovery_hints = if committed.recovery_hints.is_empty() {
                staged.recovery_hints
            } else {
                committed.recovery_hints
            };
            return Ok(CommitIfPassOutcome {
                committed: true,
                outcome: ExecOutcome {
                    revision: committed.revision,
                    branch: committed.branch,
                    summary: staged.summary,
                    created_nodes: staged.created_nodes,
                    modified_nodes: staged.modified_nodes,
                    deleted_nodes: staged.deleted_nodes,
                    solids: committed.solids,
                    recovery_hints,
                },
                report,
            });
        }
        self.discard_preview(&staging)?;
        Ok(CommitIfPassOutcome {
            committed: false,
            outcome: ExecOutcome {
                revision: main_rev,
                branch: "main".to_owned(),
                summary: staged.summary,
                created_nodes: Vec::new(),
                modified_nodes: Vec::new(),
                deleted_nodes: Vec::new(),
                solids: self.last_solids.clone(),
                recovery_hints: staged.recovery_hints,
            },
            report,
        })
    }

    fn allocate_staging_branch(&self) -> Result<String, RuntimeError> {
        let existing = self.list_branches()?;
        loop {
            let seq = COMMIT_IF_PASS_STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("preview/commit-if-pass-{}-{seq}", std::process::id());
            if existing.iter().all(|info| info.branch_name != candidate) {
                return Ok(candidate);
            }
        }
    }

    pub fn undo(&mut self) -> Result<HistoryOutcome, RuntimeError> {
        self.undo_cancellable(None)
    }

    pub fn undo_cancellable(
        &mut self,
        extra: Option<&CancelToken>,
    ) -> Result<HistoryOutcome, RuntimeError> {
        ensure_running(&self.cancel, extra)?;
        let branch = self.active_branch.clone();
        let head = head_of(&self.container, &branch)?;
        if head == 0 {
            return Err(RuntimeError::NothingToUndo);
        }
        let entries = self.container.oplog_entries(Some(&branch))?;
        let tip = entries.iter().find(|entry| seq_to_rev(entry.seq) == head);
        let Some(tip) = tip else {
            return Err(RuntimeError::NothingToUndo);
        };
        let parent = seq_to_rev(tip.parent_seq.unwrap_or(0));
        let redo = RedoEntry {
            patch: tip.patch.clone(),
            actor: tip.actor,
        };
        ensure_running(&self.cancel, extra)?;
        let graph = self
            .container
            .rollback_branch_to_seq(&branch, i64::try_from(parent).unwrap_or(0))?;
        self.redo.entry(branch.clone()).or_default().push(redo);
        self.install_active(graph, parent)?;
        Ok(HistoryOutcome {
            revision: self.revision,
            branch,
            direction: HistoryDirection::Undo,
        })
    }

    pub fn redo(&mut self) -> Result<HistoryOutcome, RuntimeError> {
        self.redo_cancellable(None)
    }

    pub fn redo_cancellable(
        &mut self,
        extra: Option<&CancelToken>,
    ) -> Result<HistoryOutcome, RuntimeError> {
        ensure_running(&self.cancel, extra)?;
        let branch = self.active_branch.clone();
        let disk_head = head_of(&self.container, &branch)?;
        if disk_head != self.revision {
            self.redo.remove(&branch);
            return Err(self.conflict(self.revision, disk_head, &branch)?);
        }
        let entry = self
            .redo
            .get_mut(&branch)
            .and_then(Vec::pop)
            .ok_or(RuntimeError::NothingToRedo)?;
        ensure_running(&self.cancel, extra)?;
        let applied = self.container.apply_patch_with_actor(
            &entry.patch,
            Some(&branch),
            entry.actor,
        );
        let entry = match applied {
            Ok(oplog) => oplog,
            Err(err) => {
                self.redo.entry(branch).or_default().push(entry);
                return Err(err.into());
            }
        };
        let graph = self.container.load_branch_graph(&branch)?;
        self.install_active(graph, seq_to_rev(entry.seq))?;
        Ok(HistoryOutcome {
            revision: self.revision,
            branch,
            direction: HistoryDirection::Redo,
        })
    }

    /// Fork `branch_name` from `main` without checking it out.
    pub fn branch_preview(&mut self, branch_name: &str) -> Result<BranchInfo, RuntimeError> {
        let name = branch_name.trim();
        if name.is_empty() {
            return Err(RuntimeError::Invalid("branch name is empty".into()));
        }
        if name == "main" {
            return Err(RuntimeError::ProtectedBranch(name.to_owned()));
        }
        let purpose = if name.starts_with("preview/")
            || name.starts_with("agent/")
            || name.starts_with("ai/")
            || name.starts_with("cand")
        {
            BranchPurpose::AiCandidate
        } else {
            BranchPurpose::User
        };
        self.container
            .create_branch_with_purpose(name, Some("main"), purpose)
            .map_err(Into::into)
    }

    pub fn list_branches(&self) -> Result<Vec<BranchInfo>, RuntimeError> {
        self.container.list_branches().map_err(Into::into)
    }

    /// Parameter, node, volume, and bounding-box differences against `main`.
    pub fn diff_branch(&mut self, branch_name: &str) -> Result<BranchDiff, RuntimeError> {
        self.require_branch(branch_name)?;
        let main_graph = self.load_graph_for("main")?;
        let branch_graph = self.load_graph_for(branch_name)?;
        let (added_parameters, removed_parameters, changed_parameters) =
            diff_parameters(&main_graph, &branch_graph);
        // `node_delta` returns (created, modified, deleted). Created nodes are
        // additions on the branch; deleted nodes are present only on main.
        let (added_nodes, changed_nodes, removed_nodes) = node_delta(&main_graph, &branch_graph);
        let after = self.evaluate_graph(&branch_graph)?.digest;
        let before = self.evaluate_graph(&main_graph)?.digest;
        self.refresh_live_solids()?;
        Ok(BranchDiff {
            branch: branch_name.to_owned(),
            base: "main".to_owned(),
            added_parameters,
            removed_parameters,
            changed_parameters,
            added_nodes,
            removed_nodes,
            changed_nodes,
            volume_mm3: MetricDelta {
                before: before.total_volume_mm3,
                after: after.total_volume_mm3,
            },
            bbox: BBoxDelta {
                before_min: before.bbox_min,
                before_max: before.bbox_max,
                after_min: after.bbox_min,
                after_max: after.bbox_max,
            },
        })
    }

    /// Fast-forward `main` with the preview when `main` has not moved since the fork.
    pub fn commit_preview(&mut self, branch_name: &str) -> Result<ExecOutcome, RuntimeError> {
        self.commit_preview_inner(branch_name, None, None)
    }

    /// Fast-forward `main` with an explicit actor recorded on the merge oplog entry.
    pub fn commit_preview_with_actor(
        &mut self,
        branch_name: &str,
        actor: Actor,
    ) -> Result<ExecOutcome, RuntimeError> {
        self.commit_preview_inner(branch_name, Some(actor), None)
    }

    pub fn commit_preview_cancellable(
        &mut self,
        branch_name: &str,
        extra: Option<&CancelToken>,
    ) -> Result<ExecOutcome, RuntimeError> {
        self.commit_preview_inner(branch_name, None, extra)
    }

    fn commit_preview_inner(
        &mut self,
        branch_name: &str,
        explicit_actor: Option<Actor>,
        extra: Option<&CancelToken>,
    ) -> Result<ExecOutcome, RuntimeError> {
        ensure_running(&self.cancel, extra)?;
        if branch_name == "main" || branch_name.trim().is_empty() {
            return Err(RuntimeError::ProtectedBranch(branch_name.to_owned()));
        }
        let preview = self
            .branch_info(branch_name)?
            .ok_or_else(|| RuntimeError::UnknownBranch(branch_name.to_owned()))?;
        let main = self
            .branch_info("main")?
            .ok_or_else(|| RuntimeError::UnknownBranch("main".to_owned()))?;
        let main_revision = seq_to_rev(main.head_seq);
        let base_revision = seq_to_rev(preview.base_seq.unwrap_or(0));
        if main_revision != base_revision {
            return Err(RuntimeError::PreviewDiverged {
                main_revision,
                base_revision,
            });
        }

        let merge_oplog_actor = match &explicit_actor {
            Some(actor) => to_oplog(actor),
            None => self
                .container
                .oplog_entries(Some(branch_name))?
                .last()
                .map(|entry| entry.actor)
                .unwrap_or_else(|| to_oplog(&self.actor)),
        };

        let main_graph = self.load_graph_for("main")?;
        let preview_graph = self.load_graph_for(branch_name)?;
        let patch_id = format!("preview-merge-{}", branch_name.replace('/', "-"));
        let mut patch = cad_kcl_bridge::diff_graphs(&main_graph, &preview_graph, &patch_id);
        if patch.ops.is_empty() {
            self.release_merged_preview_branch(branch_name, main_revision)?;
            let evaluated = self.evaluate_graph(&main_graph)?;
            let solids = evaluated.digest;
            let recovery_hints = evaluated.facts.recovery_hints;
            self.refresh_live_solids()?;
            return Ok(ExecOutcome {
                revision: main_revision,
                branch: "main".to_owned(),
                summary: ChangeSummary {
                    title: "No semantic changes".to_owned(),
                    lines: vec![format!("`{branch_name}` already matches main.")],
                    changes: Vec::new(),
                },
                created_nodes: Vec::new(),
                modified_nodes: Vec::new(),
                deleted_nodes: Vec::new(),
                solids,
                recovery_hints,
            });
        }

        patch.description = format!("commit preview {branch_name}");
        ensure_running(&self.cancel, extra)?;
        let mut staged = main_graph.clone();
        staged
            .apply_patch(&patch)
            .map_err(|err| RuntimeError::Eval(err.to_string()))?;
        let preflight = self.evaluate_graph(&staged)?;
        ensure_running(&self.cancel, extra)?;

        let main_now = head_of(&self.container, "main")?;
        if main_now != main_revision {
            return Err(RuntimeError::PreviewDiverged {
                main_revision: main_now,
                base_revision,
            });
        }
        let entry = self.container.apply_patch_with_actor(
            &patch,
            Some("main"),
            merge_oplog_actor,
        )?;
        let revision = seq_to_rev(entry.seq);
        self.release_merged_preview_branch(branch_name, revision)?;
        self.redo.remove("main");
        let persisted = self.container.load_branch_graph("main")?;
        let (created_nodes, modified_nodes, deleted_nodes) = node_delta(&main_graph, &persisted);
        let (solids, mut recovery_hints) = self.remember_branch("main", persisted, revision)?;
        if recovery_hints.is_empty() && !preflight.facts.recovery_hints.is_empty() {
            recovery_hints = preflight.facts.recovery_hints;
            if self.active_branch == "main" {
                self.eval_facts.recovery_hints = recovery_hints.clone();
            }
        }
        Ok(ExecOutcome {
            revision,
            branch: "main".to_owned(),
            summary: ChangeSummary {
                title: format!("commit preview {branch_name}"),
                lines: vec![format!("Merged `{branch_name}` into main.")],
                changes: Vec::new(),
            },
            created_nodes,
            modified_nodes,
            deleted_nodes,
            solids,
            recovery_hints,
        })
    }

    fn release_merged_preview_branch(
        &mut self,
        branch_name: &str,
        main_revision: u64,
    ) -> Result<(), RuntimeError> {
        if self.active_branch == branch_name {
            let graph = self.container.checkout_branch("main")?;
            self.active_branch = "main".to_owned();
            self.install_active(graph, main_revision)?;
        }
        self.container
            .set_branch_status(branch_name, BranchStatus::Promoted, None)?;
        self.container.delete_branch(branch_name)?;
        self.redo.remove(branch_name);
        Ok(())
    }

    /// Drop a preview branch. `main` is left as it was.
    pub fn discard_preview(&mut self, branch_name: &str) -> Result<(), RuntimeError> {
        if branch_name == "main" || branch_name.trim().is_empty() {
            return Err(RuntimeError::ProtectedBranch(branch_name.to_owned()));
        }
        self.require_branch(branch_name)?;
        if self.active_branch == branch_name {
            let graph = self.container.checkout_branch("main")?;
            let revision = head_of(&self.container, "main")?;
            self.active_branch = "main".to_owned();
            self.install_active(graph, revision)?;
        }
        self.container
            .set_branch_status(branch_name, BranchStatus::Discarded, None)?;
        self.container.delete_branch(branch_name)?;
        self.redo.remove(branch_name);
        Ok(())
    }

    pub fn emit_kcl_view(&self) -> Result<String, RuntimeError> {
        Ok(cad_kcl_bridge::emit_kcl(&self.graph)?)
    }

    pub fn sync_kcl_view(
        &mut self,
        expected_revision: Option<u64>,
        actor: Actor,
        kcl_source: &str,
    ) -> Result<ExecOutcome, RuntimeError> {
        self.sync_kcl_view_cancellable(expected_revision, actor, kcl_source, None)
    }

    pub fn sync_kcl_view_cancellable(
        &mut self,
        expected_revision: Option<u64>,
        actor: Actor,
        kcl_source: &str,
        extra: Option<&CancelToken>,
    ) -> Result<ExecOutcome, RuntimeError> {
        ensure_running(&self.cancel, extra)?;
        let branch = self.active_branch.clone();
        // A stale revision fails before the text is parsed, so bad KCL cannot sneak in.
        self.check_revision(expected_revision, &branch)?;
        let seen = head_of(&self.container, &branch)?;
        let before = self.load_graph_for(&branch)?;
        let mut parsed = cad_kcl_bridge::parse_any_kcl_to_graph(kcl_source)?;
        if let Some(contract_node) = before.nodes.get("__design_contract") {
            parsed
                .nodes
                .entry("__design_contract".to_owned())
                .or_insert_with(|| contract_node.clone());
        }
        let mut patch = cad_kcl_bridge::diff_graphs(&before, &parsed, "kcl-sync");
        if patch.ops.is_empty() {
            if seen != self.revision {
                self.install_active(before, seen)?;
            }
            self.actor = actor;
            return Ok(self.current_outcome(
                branch,
                self.revision,
                ChangeSummary {
                    title: "No semantic changes".to_owned(),
                    lines: Vec::new(),
                    changes: Vec::new(),
                },
                Vec::new(),
                Vec::new(),
                Vec::new(),
                self.last_solids.clone(),
                self.eval_facts.recovery_hints.clone(),
            ));
        }
        patch.description = "sync kcl view".to_owned();
        ensure_running(&self.cancel, extra)?;
        let mut staged = before.clone();
        staged
            .apply_patch(&patch)
            .map_err(|err| RuntimeError::Eval(err.to_string()))?;
        let preflight = self.evaluate_graph(&staged)?;
        ensure_running(&self.cancel, extra)?;
        let now = head_of(&self.container, &branch)?;
        if now != seen {
            return Err(self.conflict(seen, now, &branch)?);
        }
        let entry =
            self.container
                .apply_patch_with_actor(&patch, Some(&branch), to_oplog(&actor))?;
        self.redo.remove(&branch);
        self.actor = actor;
        let persisted = self.container.load_branch_graph(&branch)?;
        let (created_nodes, modified_nodes, deleted_nodes) = node_delta(&before, &persisted);
        let revision = seq_to_rev(entry.seq);
        let (solids, mut recovery_hints) = self.remember_branch(&branch, persisted, revision)?;
        if recovery_hints.is_empty() && !preflight.facts.recovery_hints.is_empty() {
            recovery_hints = preflight.facts.recovery_hints;
            if branch == self.active_branch {
                self.eval_facts.recovery_hints = recovery_hints.clone();
            }
        }
        Ok(self.current_outcome(
            branch,
            revision,
            ChangeSummary {
                title: "sync kcl view".to_owned(),
                lines: vec!["Applied the KCL view to the open branch.".to_owned()],
                changes: Vec::new(),
            },
            created_nodes,
            modified_nodes,
            deleted_nodes,
            solids,
            recovery_hints,
        ))
    }

    fn prepare_target(&self, actor: &Actor, target: &BranchTarget) -> Result<WriteTarget, RuntimeError> {
        match target {
            BranchTarget::Main => {
                self.require_branch("main")?;
                Ok(WriteTarget {
                    branch: "main".to_owned(),
                    fork_from_main: false,
                })
            }
            BranchTarget::Named(name) => {
                if name.trim().is_empty() {
                    return Err(RuntimeError::Invalid("branch name is empty".into()));
                }
                self.require_branch(name)?;
                Ok(WriteTarget {
                    branch: name.clone(),
                    fork_from_main: false,
                })
            }
            BranchTarget::Active => {
                if self.mode == RuntimeMode::LiveAttached {
                    if let Actor::ExternalAgent { client, session_id } = actor {
                        if client.is_empty() || session_id.is_empty() {
                            return Err(RuntimeError::Invalid(
                                "external agent client and session_id are required".into(),
                            ));
                        }
                        let branch = format!("agent/{client}/{session_id}");
                        let fork_from_main = !self.branch_exists(&branch)?;
                        return Ok(WriteTarget {
                            branch,
                            fork_from_main,
                        });
                    }
                }
                Ok(WriteTarget {
                    branch: self.active_branch.clone(),
                    fork_from_main: false,
                })
            }
        }
    }

    fn check_revision(&self, expected: Option<u64>, branch: &str) -> Result<(), RuntimeError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual = head_of(&self.container, branch)?;
        if expected != actual {
            return Err(self.conflict(expected, actual, branch)?);
        }
        Ok(())
    }

    fn conflict(
        &self,
        expected: u64,
        actual: u64,
        branch: &str,
    ) -> Result<RuntimeError, RuntimeError> {
        Ok(RuntimeError::RevisionConflict {
            expected,
            actual,
            latest_summary: latest_summary(&self.container, branch, actual)?,
        })
    }

    fn load_graph_for(&self, branch: &str) -> Result<CanonicalGraph, RuntimeError> {
        if branch == self.active_branch {
            let disk_head = head_of(&self.container, branch)?;
            if disk_head == self.revision {
                return Ok(self.graph.clone());
            }
        }
        self.container
            .load_branch_graph(branch)
            .map_err(Into::into)
    }

    fn require_branch(&self, branch: &str) -> Result<(), RuntimeError> {
        if self.branch_exists(branch)? {
            Ok(())
        } else {
            Err(RuntimeError::UnknownBranch(branch.to_owned()))
        }
    }

    fn branch_exists(&self, branch: &str) -> Result<bool, RuntimeError> {
        Ok(self.branch_info(branch)?.is_some())
    }

    fn branch_info(&self, branch: &str) -> Result<Option<BranchInfo>, RuntimeError> {
        let branches = self.container.list_branches()?;
        Ok(branches.into_iter().find(|info| info.branch_name == branch))
    }

    fn noop_outcome(
        &mut self,
        branch: &str,
        graph: &CanonicalGraph,
    ) -> Result<ExecOutcome, RuntimeError> {
        let revision = head_of(&self.container, branch)?;
        let (solids, recovery_hints) = if branch == self.active_branch {
            if revision != self.revision {
                let solids = self.install_active(graph.clone(), revision)?;
                (solids, self.eval_facts.recovery_hints.clone())
            } else {
                (
                    self.last_solids.clone(),
                    self.eval_facts.recovery_hints.clone(),
                )
            }
        } else {
            let evaluated = self.evaluate_graph(graph)?;
            let digest = evaluated.digest;
            let hints = evaluated.facts.recovery_hints;
            self.refresh_live_solids()?;
            (digest, hints)
        };
        Ok(self.current_outcome(
            branch.to_owned(),
            revision,
            ChangeSummary {
                title: "No semantic changes".to_owned(),
                lines: Vec::new(),
                changes: Vec::new(),
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            solids,
            recovery_hints,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn current_outcome(
        &self,
        branch: String,
        revision: u64,
        summary: ChangeSummary,
        created_nodes: Vec<NodeId>,
        modified_nodes: Vec<NodeId>,
        deleted_nodes: Vec<NodeId>,
        solids: SolidDigest,
        recovery_hints: Vec<NodeRecoveryHint>,
    ) -> ExecOutcome {
        ExecOutcome {
            revision,
            branch,
            summary,
            created_nodes,
            modified_nodes,
            deleted_nodes,
            solids,
            recovery_hints,
        }
    }

    /// Keep the session graph when the write landed on the checked-out branch.
    fn remember_branch(
        &mut self,
        branch: &str,
        graph: CanonicalGraph,
        revision: u64,
    ) -> Result<(SolidDigest, Vec<NodeRecoveryHint>), RuntimeError> {
        if branch == self.active_branch {
            let solids = self.install_active(graph, revision)?;
            Ok((solids, self.eval_facts.recovery_hints.clone()))
        } else {
            let evaluated = self.evaluate_graph(&graph)?;
            let digest = evaluated.digest;
            let recovery_hints = evaluated.facts.recovery_hints;
            self.refresh_live_solids()?;
            if self.topo_cache.candidates.is_empty()
                && !evaluated.snapshot.candidates.is_empty()
            {
                self.topo_cache = evaluated.snapshot;
            }
            Ok((digest, recovery_hints))
        }
    }

    fn install_active(
        &mut self,
        graph: CanonicalGraph,
        revision: u64,
    ) -> Result<SolidDigest, RuntimeError> {
        let evaluated = self.evaluate_graph(&graph);
        self.graph = graph;
        self.revision = revision;
        Ok(self.adopt(evaluated?))
    }

    fn refresh_live_solids(&mut self) -> Result<(), RuntimeError> {
        let graph = self.graph.clone();
        let product = self.evaluate_graph(&graph)?;
        self.adopt(product);
        Ok(())
    }

    fn evaluate_graph(&mut self, graph: &CanonicalGraph) -> Result<EvalProduct, RuntimeError> {
        let fingerprint = self.kernel_fingerprint();
        evaluate_graph_with_cache(
            self.kernel.as_ref(),
            &fingerprint,
            &mut self.merkle_cache,
            &self.path,
            &self.workspace_root,
            graph,
        )
    }

    fn adopt(&mut self, product: EvalProduct) -> SolidDigest {
        self.output_solids = product.solids;
        self.topo_cache = product.snapshot;
        self.eval_facts = product.facts;
        self.last_solids = product.digest.clone();
        product.digest
    }
}

fn to_oplog(actor: &Actor) -> OplogActor {
    match actor {
        Actor::HumanUi => OplogActor::HumanGui,
        Actor::BuiltinAgent | Actor::ExternalAgent { .. } => OplogActor::AiAgent,
        Actor::PythonSdk => OplogActor::PythonSdk,
        Actor::KclView => OplogActor::KclSync,
    }
}

fn seq_to_rev(seq: i64) -> u64 {
    u64::try_from(seq).unwrap_or(0)
}

fn head_of(container: &SqliteContainer, branch: &str) -> Result<u64, RuntimeError> {
    let branches = container.list_branches()?;
    let info = branches
        .into_iter()
        .find(|item| item.branch_name == branch)
        .ok_or_else(|| RuntimeError::UnknownBranch(branch.to_owned()))?;
    Ok(seq_to_rev(info.head_seq))
}

fn latest_summary(
    container: &SqliteContainer,
    branch: &str,
    revision: u64,
) -> Result<String, RuntimeError> {
    if revision == 0 {
        return Ok(format!(
            "branch {branch} is at the baseline (revision 0)"
        ));
    }
    let entries = container.oplog_entries(Some(branch))?;
    if let Some(entry) = entries
        .iter()
        .find(|entry| seq_to_rev(entry.seq) == revision)
    {
        return Ok(entry.description.clone());
    }
    Ok(format!("branch {branch} is at revision {revision}"))
}

fn diff_parameters(
    before: &CanonicalGraph,
    after: &CanonicalGraph,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (name, parameter) in &before.parameters {
        match after.parameters.get(name) {
            None => removed.push(name.clone()),
            Some(other) if other != parameter => changed.push(name.clone()),
            Some(_) => {}
        }
    }
    for name in after.parameters.keys() {
        if !before.parameters.contains_key(name) {
            added.push(name.clone());
        }
    }
    added.sort();
    removed.sort();
    changed.sort();
    (added, removed, changed)
}

/// `(created, modified, deleted)` node ids. `before` is the older graph.
fn node_delta(
    before: &CanonicalGraph,
    after: &CanonicalGraph,
) -> (Vec<NodeId>, Vec<NodeId>, Vec<NodeId>) {
    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();
    for (id, node) in &before.nodes {
        match after.nodes.get(id) {
            None => deleted.push(node.id.clone()),
            Some(other) if other != node => modified.push(node.id.clone()),
            Some(_) => {}
        }
    }
    for (id, node) in &after.nodes {
        if !before.nodes.contains_key(id) {
            created.push(node.id.clone());
        }
    }
    created.sort();
    modified.sort();
    deleted.sort();
    (created, modified, deleted)
}

struct EvalProduct {
    digest: SolidDigest,
    solids: BTreeMap<NodeId, SolidShape>,
    snapshot: TopoSnapshot,
    facts: EvalFacts,
}

#[derive(Clone, Debug)]
struct EvalFacts {
    failed_ids: BTreeSet<String>,
    node_states: BTreeMap<String, NodeState>,
    recovery_hints: Vec<NodeRecoveryHint>,
    experimental: bool,
    fea_declared: bool,
    sketches: Vec<SketchFact>,
    mates_converged: Option<bool>,
    conflicting_mates: Vec<String>,
    unsatisfied_mates: usize,
}

#[derive(Clone, Debug)]
struct SketchFact {
    node_id: String,
    status: SolveStatus,
    remaining_dof: i64,
}

/// `Fast` commits when these three layers pass. L3 and L4 may stay unknown.
pub fn fast_verification_passes(report: &VerificationReport) -> bool {
    [
        VerificationLayer::L0StructureUnits,
        VerificationLayer::L1GeometryTopology,
        VerificationLayer::L2InterfacesConstraints,
    ]
    .into_iter()
    .all(|layer| report.layer_status(layer) == TriVerdict::Pass)
}

fn evaluate_graph_with_cache(
    kernel: &dyn GeometryKernel,
    kernel_fingerprint: &KernelFingerprint,
    merkle_cache: &mut MerkleEvalCache,
    parent_path: &Path,
    workspace_root: &Path,
    graph: &CanonicalGraph,
) -> Result<EvalProduct, RuntimeError> {
    let mut evaluator = GraphEvaluator::with_merkle_cache(kernel, std::mem::take(merkle_cache));
    evaluator.set_kernel_fingerprint(kernel_fingerprint.clone());
    evaluator.set_external_ref_roots(
        Some(parent_path.to_path_buf()),
        Some(workspace_root.to_path_buf()),
    );
    let eval_result = evaluator.evaluate(graph);
    let mut candidates = Vec::new();
    let mut terminal_ids: BTreeSet<NodeId> = BTreeSet::new();
    if let Ok(ref report) = eval_result {
        let consumed = consumed_solid_ids(graph, &report.output_solids);
        let has_terminal = report
            .output_solids
            .keys()
            .any(|id| !consumed.contains(id));
        for (id, solid) in &report.output_solids {
            if has_terminal && consumed.contains(id) {
                continue;
            }
            terminal_ids.insert(id.clone());
            let cached = evaluator
                .cache()
                .get(id)
                .and_then(|snapshot| snapshot.topo_snapshot.clone())
                .filter(|snapshot| !snapshot.candidates.is_empty());
            let snapshot = cached.unwrap_or_else(|| snapshot_from_solid(id, solid));
            candidates.extend(snapshot.candidates);
        }
    }
    *merkle_cache = std::mem::take(evaluator.merkle_cache_mut());
    let report = eval_result.map_err(|err| RuntimeError::Eval(err.to_string()))?;
    let mut volume = 0.0;
    let mut bbox = BBox3::empty();
    let mut output_count = 0usize;
    for (id, solid) in &report.output_solids {
        if !terminal_ids.is_empty() && !terminal_ids.contains(id) {
            continue;
        }
        output_count += 1;
        let check = kernel.check_solid(solid);
        volume += check.volume_mm3;
        bbox = bbox.union(check.bbox_mm);
    }
    let (bbox_min, bbox_max) = if bbox.is_valid() {
        (bbox.min, bbox.max)
    } else {
        ([0.0; 3], [0.0; 3])
    };
    let valid_nodes = report
        .node_reports
        .values()
        .filter(|item| item.status == NodeState::Valid)
        .count();
    let digest = SolidDigest {
        output_count,
        valid_nodes,
        total_volume_mm3: volume,
        bbox_min,
        bbox_max,
        cache_hits: report.cache_hits,
        cache_misses: report.cache_misses,
        recomputed_nodes: report.recomputed_nodes,
    };
    let failed_ids = report
        .node_reports
        .iter()
        .filter(|(id, item)| {
            matches!(item.status, NodeState::FailedWithRollback { .. })
                && !graph.nodes.get(id.0.as_str()).is_some_and(|node| {
                    matches!(
                        node.try_feature_spec(),
                        Ok(FeatureSpec::DesignAssertion { .. } | FeatureSpec::TagTopology { .. })
                    )
                })
        })
        .map(|(id, _)| id.0.clone())
        .collect();
    let experimental = graph.nodes.values().any(|node| is_experimental(&node.kind))
        || report.has_experimental_outputs();
    let fea_declared = graph
        .nodes
        .values()
        .any(|node| matches!(node.kind, NodeKind::FieldFromFeaBlob));
    let sketches = report
        .sketch_reports
        .iter()
        .map(|(id, summary)| SketchFact {
            node_id: id.0.clone(),
            status: summary.status,
            remaining_dof: summary.remaining_dof,
        })
        .collect();
    let mates_converged = report
        .assembly_solve_report
        .as_ref()
        .map(|solve| solve.converged);
    let conflicting_mates = report
        .assembly_solve_report
        .as_ref()
        .map(|solve| solve.conflicting_mates.clone())
        .unwrap_or_default();
    let unsatisfied_mates = report
        .output_solids
        .values()
        .flat_map(|solid| solid.mates.iter())
        .filter(|mate| !mate.satisfied)
        .count();
    let node_states = report
        .node_reports
        .iter()
        .map(|(id, item)| (id.0.clone(), item.status.clone()))
        .collect();
    let recovery_hints = report
        .node_reports
        .iter()
        .filter_map(|(id, item)| {
            item.recovery_hint.as_ref().and_then(|hint| {
                serde_json::to_value(hint).ok().map(|value| NodeRecoveryHint {
                    node_id: id.clone(),
                    hint: value,
                })
            })
        })
        .collect();
    Ok(EvalProduct {
        digest,
        solids: report.output_solids,
        snapshot: TopoSnapshot::new(candidates),
        facts: EvalFacts {
            failed_ids,
            node_states,
            recovery_hints,
            experimental,
            fea_declared,
            sketches,
            mates_converged,
            conflicting_mates,
            unsatisfied_mates,
        },
    })
}

fn is_experimental(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::SubDFromBlob
            | NodeKind::SubDToNurbs
            | NodeKind::FieldTpms { .. }
            | NodeKind::FieldSdf
            | NodeKind::ImplicitSmoothBlend
            | NodeKind::ImplicitLatticeInfill
    )
}

fn snapshot_from_solid(id: &NodeId, solid: &SolidShape) -> TopoSnapshot {
    propagate_lineage(
        &id.0,
        &[],
        &solid.faces.iter().map(face_sample).collect::<Vec<_>>(),
        &solid.edges.iter().map(edge_sample).collect::<Vec<_>>(),
        &EvolutionOptions::default(),
    )
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
    let points = edge.polyline.chunks_exact(3);
    let point_count = points.len().max(1) as f64;
    let mut centroid = [0.0; 3];
    let mut length = 0.0;
    let mut previous: Option<[f64; 3]> = None;
    for point in points {
        let point = [point[0], point[1], point[2]];
        for (center, coordinate) in centroid.iter_mut().zip(point) {
            *center += coordinate / point_count;
        }
        if let Some(previous) = previous {
            length += previous
                .iter()
                .zip(point)
                .map(|(left, right)| (left - right).powi(2))
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

fn export_artifact(
    session: &DocumentSession,
    format: &str,
    target_path: &Path,
    part_label: Option<&str>,
    node_id: Option<&str>,
) -> Result<ExportArtifactReport, RuntimeError> {
    let format = canonical_format(format)?;
    let requested_path = target_path.display().to_string();
    let resolved = resolve_in_workspace(&session.workspace_root, target_path)?;
    let label = part_label.map(str::trim).filter(|label| !label.is_empty());
    let node = node_id.map(str::trim).filter(|node| !node.is_empty());
    let selected = select_solids(&session.graph, &session.output_solids, label, node)?;
    let authoritative = session.is_authoritative_occt();
    if format == "step_ap242" && !authoritative {
        return Err(RuntimeError::ValidationFailed(format!(
            "authoritative OCCT kernel is required for STEP AP242 export; active backend `{}` is non-authoritative and will not emit faceted geometry as exact STEP AP242",
            session.kernel_backend_name()
        )));
    }
    let (geometry_bytes, preserved, not_preserved) = fidelity(format, authoritative);
    let kernel = session.kernel.as_ref();
    let selected = order_selected_by_graph_outputs(&session.graph, selected);
    let (exported_node_id, bytes) = match format {
        "3mf" => (
            export_scope_ids(&selected),
            mesh_bytes(&selected, label.unwrap_or(&session.project_name))?,
        ),
        "dxf" => export_drawing_bytes(&selected, label, kernel, drawing_dxf)?,
        "svg" => export_drawing_bytes(&selected, label, kernel, drawing_svg)?,
        _ => export_step_bytes(&selected, label.unwrap_or(&session.project_name), kernel)?,
    };
    crate::write_file_atomic(&resolved, &bytes)?;
    Ok(ExportArtifactReport {
        format: format.to_owned(),
        requested_path,
        resolved_path: resolved.display().to_string(),
        project_path: session.path.display().to_string(),
        revision: session.revision,
        branch: session.active_branch.clone(),
        bytes_written: bytes.len() as u64,
        sha256: crate::discovery::sha256_hex(&bytes),
        kernel_backend: session.kernel_backend_name().to_owned(),
        kernel_authoritative: authoritative,
        geometry_bytes: geometry_bytes.to_owned(),
        preserved: owned_words(preserved),
        not_preserved: owned_words(not_preserved),
        exported_node_id,
        part_label: label.map(str::to_owned),
    })
}

fn canonical_format(raw: &str) -> Result<&'static str, RuntimeError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "step_ap242" | "step" | "step ap242" | "step-ap242" | "stp" => Ok("step_ap242"),
        "3mf" | "three_mf" => Ok("3mf"),
        "dxf" => Ok("dxf"),
        "svg" | "hlr_svg" => Ok("svg"),
        _ => Err(RuntimeError::Invalid(
            "format must be step_ap242, 3mf, dxf, or svg".into(),
        )),
    }
}

fn fidelity(
    format: &str,
    authoritative: bool,
) -> (&'static str, &'static [&'static str], &'static [&'static str]) {
    match format {
        "3mf" => (
            "MESH",
            &["triangle_mesh", "units_mm"],
            &["exact_brep", "pmi", "materials", "colors", "topology_ids"],
        ),
        "dxf" | "svg" if authoritative => (
            "EXACT",
            &["drawing_views", "units_mm"],
            &["solid_brep", "pmi", "materials", "colors", "assembly_mates"],
        ),
        "dxf" | "svg" => (
            "APPROXIMATE",
            &["drawing_views", "units_mm"],
            &[
                "exact_brep",
                "solid_brep",
                "pmi",
                "materials",
                "colors",
                "assembly_mates",
            ],
        ),
        _ => (
            "EXACT",
            &["exact_brep", "topology", "units_mm", "part_name"],
            &[
                "pmi",
                "materials",
                "colors",
                "assembly_mates",
                "design_contract",
            ],
        ),
    }
}

fn owned_words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn collect_node_refs(value: &IrValue, out: &mut BTreeSet<NodeId>) {
    match value {
        IrValue::NodeRef { node_id, .. } => {
            out.insert(node_id.clone());
        }
        IrValue::List(items) => {
            for item in items {
                collect_node_refs(item, out);
            }
        }
        _ => {}
    }
}

fn consumed_solid_ids(
    graph: &CanonicalGraph,
    solids: &BTreeMap<NodeId, SolidShape>,
) -> BTreeSet<NodeId> {
    let mut consumed = BTreeSet::new();
    for id in solids.keys() {
        let Some(node) = graph.nodes.get(id.0.as_str()) else {
            continue;
        };
        if matches!(node.state, NodeState::Suppressed) {
            continue;
        }
        for value in node.inputs.values() {
            collect_node_refs(value, &mut consumed);
        }
    }
    consumed
}

fn select_solids<'a>(
    graph: &CanonicalGraph,
    solids: &'a BTreeMap<NodeId, SolidShape>,
    label: Option<&str>,
    node_id: Option<&str>,
) -> Result<Vec<(&'a NodeId, &'a SolidShape)>, RuntimeError> {
    if solids.is_empty() {
        return Err(RuntimeError::ValidationFailed(
            "document has no evaluated solid".into(),
        ));
    }
    let Some(node_id) = node_id else {
        return select_by_part_label(graph, solids, label);
    };
    let explicit = select_explicit_node(solids, node_id)?;
    if let Some(label) = label {
        let by_name = select_by_part_label(graph, solids, Some(label))?;
        if by_name[0].0 != explicit[0].0 {
            return Err(RuntimeError::ValidationFailed(format!(
                "part_label `{label}` resolves to finished solid `{}`, but node_id `{node_id}` is `{}`. Pass only one.",
                by_name[0].0, explicit[0].0
            )));
        }
    }
    Ok(explicit)
}

fn select_by_part_label<'a>(
    graph: &CanonicalGraph,
    solids: &'a BTreeMap<NodeId, SolidShape>,
    label: Option<&str>,
) -> Result<Vec<(&'a NodeId, &'a SolidShape)>, RuntimeError> {
    let consumed = consumed_solid_ids(graph, solids);
    let terminal: Vec<_> = solids
        .iter()
        .filter(|(id, _)| !consumed.contains(*id))
        .collect();
    let Some(label) = label else {
        if !terminal.is_empty() {
            return Ok(terminal);
        }
        return Ok(solids.iter().collect());
    };
    let hits: Vec<NodeId> = solids
        .iter()
        .filter(|(id, solid)| label_matches(graph, id, solid, label))
        .map(|(id, _)| (*id).clone())
        .collect();
    if hits.is_empty() {
        return Err(RuntimeError::ValidationFailed(format!(
            "no evaluated solid matches part label `{label}`"
        )));
    }
    let consumers = solid_consumers(graph, solids);
    let mut finished = BTreeSet::new();
    let mut stuck = Vec::new();
    for id in &hits {
        if !consumed.contains(id) {
            finished.insert(id.clone());
            continue;
        }
        let downstream = terminal_descendants(id, &consumed, &consumers);
        if downstream.is_empty() {
            stuck.push(id.0.clone());
        } else {
            finished.extend(downstream);
        }
    }
    if finished.is_empty() {
        return Err(RuntimeError::ValidationFailed(format!(
            "part label `{label}` matches upstream feature {} but no finished solid. Pass node_id to export that feature explicitly.",
            stuck.join(", ")
        )));
    }
    if finished.len() > 1 {
        let listed = finished
            .iter()
            .map(|id| match solids.get(id).map(|solid| solid.part_name.as_str()) {
                Some(name) if !name.is_empty() => format!("{} (name {name})", id.0),
                _ => id.0.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(RuntimeError::ValidationFailed(format!(
            "part label `{label}` matches more than one finished solid: {listed}. Pass node_id to choose one."
        )));
    }
    let id = finished.iter().next().expect("one finished solid");
    let (id, solid) = solids.get_key_value(id).expect("finished solid is evaluated");
    Ok(vec![(id, solid)])
}

fn label_matches(
    graph: &CanonicalGraph,
    id: &NodeId,
    solid: &SolidShape,
    label: &str,
) -> bool {
    if id.0 == label {
        return true;
    }
    if !solid.part_name.is_empty() && solid.part_name.eq_ignore_ascii_case(label) {
        return true;
    }
    graph
        .nodes
        .get(id.0.as_str())
        .is_some_and(|node| !node.name.is_empty() && node.name.eq_ignore_ascii_case(label))
}

fn select_explicit_node<'a>(
    solids: &'a BTreeMap<NodeId, SolidShape>,
    node_id: &str,
) -> Result<Vec<(&'a NodeId, &'a SolidShape)>, RuntimeError> {
    if let Some((id, solid)) = solids.iter().find(|(id, _)| id.0 == node_id) {
        return Ok(vec![(id, solid)]);
    }
    let matches: Vec<_> = solids
        .iter()
        .filter(|(id, _)| id.0.eq_ignore_ascii_case(node_id))
        .collect();
    if matches.len() == 1 {
        return Ok(matches);
    }
    if matches.len() > 1 {
        let ids = matches
            .iter()
            .map(|(id, _)| id.0.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(RuntimeError::ValidationFailed(format!(
            "node_id `{node_id}` matches more than one node: {ids}"
        )));
    }
    Err(RuntimeError::ValidationFailed(format!(
        "no evaluated solid has node_id `{node_id}`"
    )))
}

fn solid_consumers(
    graph: &CanonicalGraph,
    solids: &BTreeMap<NodeId, SolidShape>,
) -> BTreeMap<NodeId, Vec<NodeId>> {
    let mut index: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for id in solids.keys() {
        let Some(node) = graph.nodes.get(id.0.as_str()) else {
            continue;
        };
        if matches!(node.state, NodeState::Suppressed) {
            continue;
        }
        let mut referenced = BTreeSet::new();
        for value in node.inputs.values() {
            collect_node_refs(value, &mut referenced);
        }
        for source in referenced {
            if solids.contains_key(&source) {
                index.entry(source).or_default().push(id.clone());
            }
        }
    }
    index
}

fn terminal_descendants(
    start: &NodeId,
    consumed: &BTreeSet<NodeId>,
    consumers: &BTreeMap<NodeId, Vec<NodeId>>,
) -> BTreeSet<NodeId> {
    let mut finished = BTreeSet::new();
    let mut stack = vec![start.clone()];
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(next) = consumers.get(&id) else {
            continue;
        };
        for child in next {
            if consumed.contains(child) {
                stack.push(child.clone());
            } else {
                finished.insert(child.clone());
            }
        }
    }
    finished
}


fn order_selected_by_graph_outputs<'a>(
    graph: &CanonicalGraph,
    mut selected: Vec<(&'a NodeId, &'a SolidShape)>,
) -> Vec<(&'a NodeId, &'a SolidShape)> {
    selected.sort_by_key(|(id, _)| {
        graph
            .outputs
            .iter()
            .position(|out| out == *id)
            .unwrap_or(usize::MAX)
    });
    selected
}

fn export_scope_ids(selected: &[(&NodeId, &SolidShape)]) -> Option<String> {
    if selected.is_empty() {
        return None;
    }
    Some(
        selected
            .iter()
            .map(|(id, _)| id.0.as_str())
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn grouped_finished_solid(selected: &[(&NodeId, &SolidShape)], name: &str) -> SolidShape {
    if let [(_, solid)] = selected {
        return (*solid).clone();
    }
    let mut group = SolidShape::empty(name);
    let mut csg_items = Vec::with_capacity(selected.len());
    for (id, solid) in selected {
        let mut placed = (*solid).clone();
        let cid = id.0.clone();
        let part_name = if placed.part_name.is_empty() {
            cid.clone()
        } else {
            placed.part_name.clone()
        };
        placed.part_name = part_name.clone();
        for face in &mut placed.faces {
            face.component_id = Some(cid.clone());
            face.face_id = format!("{cid}_{}", face.face_id);
        }
        for edge in &mut placed.edges {
            edge.component_id = Some(cid.clone());
            edge.edge_id = format!("{cid}_{}", edge.edge_id);
        }
        group.components.push(ComponentMeta {
            component_id: cid,
            part_name,
            origin_mm: [0.0; 3],
            rotation_deg: [0.0; 3],
            volume_mm3: 0.0,
            bbox_mm: BBox3::default(),
        });
        group.exact_brep &= placed.exact_brep;
        csg_items.push(placed.csg);
        group.faces.extend(placed.faces);
        group.edges.extend(placed.edges);
        group.pmi.extend(placed.pmi);
    }
    group.csg = CsgNode::Union(csg_items);
    group.payload = KernelPayload::Analytic;
    group.part_name = name.to_owned();
    group
}

fn export_step_bytes(
    selected: &[(&NodeId, &SolidShape)],
    name: &str,
    kernel: &dyn GeometryKernel,
) -> Result<(Option<String>, Vec<u8>), RuntimeError> {
    let ids = export_scope_ids(selected);
    if selected.len() == 1 {
        return Ok((ids, text_bytes(&step_bytes(kernel, selected[0])?)));
    }
    let group = grouped_finished_solid(selected, name);
    Ok((
        ids,
        text_bytes(&kernel.export_step(&group).map_err(RuntimeError::Eval)?),
    ))
}

fn export_drawing_bytes(
    selected: &[(&NodeId, &SolidShape)],
    label: Option<&str>,
    kernel: &dyn GeometryKernel,
    render: fn(&dyn GeometryKernel, (&NodeId, &SolidShape)) -> Result<String, RuntimeError>,
) -> Result<(Option<String>, Vec<u8>), RuntimeError> {
    let ids = export_scope_ids(selected);
    if selected.len() == 1 {
        return Ok((ids, text_bytes(&render(kernel, selected[0])?)));
    }
    let name = label.unwrap_or("export");
    let group = grouped_finished_solid(selected, name);
    let group_id = NodeId::from(name);
    Ok((ids, text_bytes(&render(kernel, (&group_id, &group))?)))
}

fn primary_solid<'a>(
    graph: &CanonicalGraph,
    kernel: &dyn GeometryKernel,
    solids: &[(&'a NodeId, &'a SolidShape)],
) -> Result<(&'a NodeId, &'a SolidShape), RuntimeError> {
    if solids.is_empty() {
        return Err(RuntimeError::ValidationFailed(
            "document has no evaluated solid".into(),
        ));
    }
    let output_rank = |id: &NodeId| -> usize {
        graph
            .outputs
            .iter()
            .position(|out| out == id)
            .unwrap_or(usize::MAX)
    };
    let mut best = 0usize;
    let mut best_key: (bool, usize, f64) = (false, 0, f64::NEG_INFINITY);
    for (index, (id, solid)) in solids.iter().enumerate() {
        let volume = kernel.check_solid(solid).volume_mm3;
        let positive = volume.is_finite() && volume > 0.0;
        let rank = output_rank(id);
        let key = (
            positive,
            rank,
            if positive { volume } else { f64::NEG_INFINITY },
        );
        if key >= best_key {
            best_key = key;
            best = index;
        }
    }
    Ok(solids[best])
}

fn step_bytes(kernel: &dyn GeometryKernel, solid: (&NodeId, &SolidShape)) -> Result<String, RuntimeError> {
    kernel.export_step(solid.1).map_err(RuntimeError::Eval)
}

fn drawing_dxf(
    kernel: &dyn GeometryKernel,
    solid: (&NodeId, &SolidShape),
) -> Result<String, RuntimeError> {
    let check = kernel.check_solid(solid.1);
    cad_render::render_dxf(solid.1, &check).map_err(RuntimeError::Eval)
}

fn drawing_svg(
    kernel: &dyn GeometryKernel,
    solid: (&NodeId, &SolidShape),
) -> Result<String, RuntimeError> {
    let check = kernel.check_solid(solid.1);
    cad_render::try_render_drawing_svg(solid.1, &check, cad_render::DrawingOptions::default())
        .map_err(RuntimeError::Eval)
}

fn text_bytes(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Encode a single evaluated solid as a 3MF archive in memory.
pub fn encode_solid_3mf(solid: &SolidShape, model_name: &str) -> Result<Vec<u8>, RuntimeError> {
    let export_id = NodeId::from("export");
    mesh_bytes(&[(&export_id, solid)], model_name)
}

fn mesh_bytes(
    solids: &[(&NodeId, &SolidShape)],
    model_name: &str,
) -> Result<Vec<u8>, RuntimeError> {
    let mut mesh = Mesh::default();
    for (_, solid) in solids {
        append_solid_mesh(&mut mesh, solid);
    }
    mesh.strip_degenerate_triangles();
    mesh.weld_vertices(1.0e-6);
    if mesh.triangles.is_empty() {
        return Err(RuntimeError::ValidationFailed(
            "evaluated solid has no triangles to write as 3MF".into(),
        ));
    }
    cad_mesh_io::write_3mf_mesh_bytes(&mesh, &xml_model_name(model_name))
        .map_err(|err| RuntimeError::Eval(err.to_string()))
}

fn append_solid_mesh(mesh: &mut Mesh, solid: &SolidShape) {
    for face in &solid.faces {
        let base = mesh.vertices.len() as u32;
        let count = face.vertices.len() / 3;
        for point in face.vertices.chunks_exact(3) {
            mesh.vertices.push([point[0], point[1], point[2]]);
        }
        for tri in face.indices.chunks_exact(3) {
            if tri.iter().all(|index| (*index as usize) < count) {
                mesh.triangles
                    .push([base + tri[0], base + tri[1], base + tri[2]]);
            }
        }
    }
}

fn xml_model_name(raw: &str) -> String {
    let mut name = String::new();
    for character in raw.chars() {
        let code = character as u32;
        if (code < 0x20 && !matches!(character, '\t' | '\n' | '\r'))
            || code == 0xFFFE
            || code == 0xFFFF
        {
            continue;
        }
        name.push(character);
    }
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "part".to_owned()
    } else {
        trimmed.to_owned()
    }
}

struct VerifyView<'a> {
    graph: &'a CanonicalGraph,
    output_solids: &'a BTreeMap<NodeId, SolidShape>,
    eval_facts: &'a EvalFacts,
    kernel: &'a dyn GeometryKernel,
    kernel_identity: &'a KernelIdentity,
}

fn verify_view(view: &VerifyView<'_>, profile: VerificationProfile) -> VerificationReport {
    let mut checks = vec![
        structure_check(view),
        geometry_check(view),
        interface_checks(view),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if profile == VerificationProfile::Release {
        checks.extend(manufacturing_checks(view));
        checks.push(roundtrip_check(view));
    }
    VerificationReport::from_checks(
        view.kernel_identity.clone(),
        checks,
        Some(
            "Fast records L0-L2. Release also records L3 and L4. Unknown is not Pass.".to_owned(),
        ),
    )
}

fn structure_check(view: &VerifyView<'_>) -> Vec<VerificationCheck> {
    let summary = inspect_document_summary(view.graph);
    let ir = validate_graph(view.graph);
    let check = if summary.validation.valid && ir.is_ok() {
        VerificationCheck::new(
            "L0_STRUCTURE",
            VerificationLayer::L0StructureUnits,
            TriVerdict::Pass,
            "cad-api and cad-ir graph validation passed",
        )
    } else {
        let message = summary
            .validation
            .message
            .clone()
            .or_else(|| ir.err().map(|err| err.to_string()))
            .unwrap_or_else(|| "graph validation failed".to_owned());
        VerificationCheck::new(
            "L0_STRUCTURE",
            VerificationLayer::L0StructureUnits,
            TriVerdict::Fail,
            message,
        )
    };
    vec![check]
}

fn geometry_check(view: &VerifyView<'_>) -> Vec<VerificationCheck> {
    if view.eval_facts.experimental {
        return vec![VerificationCheck::new(
            "L1_EXPERIMENTAL",
            VerificationLayer::L1GeometryTopology,
            TriVerdict::Unknown,
            "experimental SubD, TPMS, or implicit geometry is not certified as exact B-Rep",
        )];
    }
    if !view.eval_facts.failed_ids.is_empty() {
        return vec![VerificationCheck::new(
            "L1_ROLLBACK",
            VerificationLayer::L1GeometryTopology,
            TriVerdict::Fail,
            format!(
                "evaluation rolled back nodes: {}",
                view.eval_facts
                    .failed_ids
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )];
    }
    if !view.output_solids.is_empty() {
        let kernel = view.kernel;
        let mut issues = Vec::new();
        for (id, solid) in view.output_solids {
            if let Some(issue) = solid_issue(kernel, id, solid) {
                issues.push(issue);
            }
        }
        if issues.is_empty() {
            let volume = primary_volume(view.graph, kernel, view.output_solids).unwrap_or(0.0);
            return vec![VerificationCheck::new(
                "L1_SOLIDS",
                VerificationLayer::L1GeometryTopology,
                TriVerdict::Pass,
                "evaluated solids are watertight with positive finite volume",
            )
            .with_evidence("output_count", view.output_solids.len() as u64)
            .with_evidence("primary_volume_mm3", volume)];
        }
        return vec![VerificationCheck::new(
            "L1_SOLIDS",
            VerificationLayer::L1GeometryTopology,
            TriVerdict::Fail,
            issues.join(" | "),
        )];
    }
    let summary = inspect_document_summary(view.graph);
    if summary.feature_outline.iter().any(|entry| is_body(&entry.feature)) {
        return vec![VerificationCheck::new(
            "L1_SOLIDS",
            VerificationLayer::L1GeometryTopology,
            TriVerdict::Fail,
            "feature tree names a body but the runtime has no positive solid",
        )];
    }
    vec![VerificationCheck::new(
        "L1_SOLIDS",
        VerificationLayer::L1GeometryTopology,
        TriVerdict::Unknown,
        "no evaluated solid is available to judge",
    )]
}

fn solid_issue(kernel: &dyn GeometryKernel, id: &NodeId, solid: &SolidShape) -> Option<String> {
    let check = kernel.check_solid(solid);
    let volume_ok = check.volume_mm3.is_finite() && check.volume_mm3 > 0.0;
    if check.watertight && check.open_edge_count == 0 && volume_ok {
        return None;
    }
    let detail = if check.issues.is_empty() {
        format!(
            "watertight={} open_edges={} volume_mm3={}",
            check.watertight, check.open_edge_count, check.volume_mm3
        )
    } else {
        check.issues.join("; ")
    };
    Some(format!("{}: {detail}", id.0))
}

fn is_body(feature: &str) -> bool {
    matches!(
        feature,
        "primitive_box"
            | "primitive_cylinder"
            | "extrude"
            | "revolve"
            | "sweep"
            | "loft"
            | "hole"
            | "std_fastener_hole"
            | "boolean"
            | "shell"
            | "fillet"
            | "chamfer"
    )
}

fn interface_checks(view: &VerifyView<'_>) -> Vec<VerificationCheck> {
    let mut checks = Vec::new();
    for node in view.graph.nodes.values() {
        let Ok(FeatureSpec::DesignAssertion {
            target,
            assertion_id,
            kind,
        }) = node.try_feature_spec()
        else {
            continue;
        };
        if is_design_contract(&node.id.0, &kind) {
            continue;
        }
        let mut visited = BTreeSet::new();
        visited.insert(node.id.0.clone());
        checks.push(assertion_check(
            view,
            target.as_ref(),
            &assertion_id,
            &kind,
            &mut visited,
        ));
    }
    for sketch in &view.eval_facts.sketches {
        checks.push(sketch_check(sketch));
    }
    if let Some(check) = mate_check(view) {
        checks.push(check);
    }
    if let Some(contract) = inspect_document_summary(view.graph).design_contract {
        let mut check = if contract.attributes.is_empty() {
            VerificationCheck::new(
                "L2_CONTRACT",
                VerificationLayer::L2InterfacesConstraints,
                TriVerdict::Pass,
                "design contract metadata is recorded with no unverified constraint attributes",
            )
        } else {
            let keys: Vec<String> = contract.attributes.keys().cloned().collect();
            VerificationCheck::new(
                "L2_CONTRACT",
                VerificationLayer::L2InterfacesConstraints,
                TriVerdict::Unknown,
                "design contract declares constraint attributes that are not geometrically verified by this runtime",
            )
            .with_evidence("unverified_attributes", keys)
        };
        check = check.with_evidence("part_name", contract.part_name);
        checks.push(check);
    }
    if checks.is_empty() {
        checks.push(VerificationCheck::new(
            "L2_CLEAR",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Pass,
            "no interface violations and no unverified external assertions",
        ));
    }
    checks
}

fn is_design_contract(node_id: &str, kind: &DesignAssertionKind) -> bool {
    if node_id == "__design_contract" {
        return true;
    }
    match kind {
        DesignAssertionKind::Custom { expression, .. } => {
            serde_json::from_str::<DesignContract>(expression).is_ok()
        }
        _ => false,
    }
}

fn assertion_check(
    view: &VerifyView<'_>,
    target: Option<&NodeId>,
    assertion_id: &str,
    kind: &DesignAssertionKind,
    visited: &mut BTreeSet<String>,
) -> VerificationCheck {
    match kind {
        DesignAssertionKind::VolumeBounds { min_mm3, max_mm3 } => {
            let resolved_volume = match target {
                Some(target_id) => view.output_solids.get(target_id).and_then(|solid| {
                    let volume = view.kernel.check_solid(solid).volume_mm3;
                    (volume.is_finite() && volume > 0.0).then_some(volume)
                }),
                None => primary_volume(view.graph, view.kernel, view.output_solids),
            };
            match resolved_volume {
                Some(volume) => {
                    let below = min_mm3.is_some_and(|min| volume + 1.0e-6 < min);
                    let above = max_mm3.is_some_and(|max| volume - 1.0e-6 > max);
                    let mut check = if below || above {
                        VerificationCheck::new(
                            "L2_VOLUME",
                            VerificationLayer::L2InterfacesConstraints,
                            TriVerdict::Fail,
                            format!(
                                "assertion `{assertion_id}` volume {volume} mm^3 is outside bounds"
                            ),
                        )
                    } else {
                        VerificationCheck::new(
                            "L2_VOLUME",
                            VerificationLayer::L2InterfacesConstraints,
                            TriVerdict::Pass,
                            format!("assertion `{assertion_id}` volume is inside bounds"),
                        )
                    }
                    .with_evidence("volume_mm3", volume);
                    if let Some(target_id) = target {
                        check = check.with_evidence("target", target_id.0.clone());
                    }
                    check
                }
                None => {
                    let mut check = VerificationCheck::new(
                        "L2_VOLUME",
                        VerificationLayer::L2InterfacesConstraints,
                        TriVerdict::Fail,
                        match target {
                            Some(target_id) => format!(
                                "assertion `{assertion_id}` target `{}` has no positive solid volume",
                                target_id.0
                            ),
                            None => format!(
                                "assertion `{assertion_id}` has no positive solid volume"
                            ),
                        },
                    );
                    if let Some(target_id) = target {
                        check = check.with_evidence("target", target_id.0.clone());
                    }
                    check
                }
            }
        }
        DesignAssertionKind::MinWallThickness { min } => match length_mm(view.graph, min) {
            Some(min_mm) => {
                let (status, message, summary) = match target {
                    Some(target_id) => match view.output_solids.get(target_id) {
                        Some(solid) => {
                            crate::l3_dfm::evaluate_min_wall_check(&[(target_id, solid)], min_mm)
                        }
                        None => (
                            TriVerdict::Fail,
                            format!("target `{}` has no evaluated solid for DFM", target_id.0),
                            None,
                        ),
                    },
                    None => {
                        let terminal = select_solids(view.graph, view.output_solids, None, None)
                            .unwrap_or_default();
                        crate::l3_dfm::evaluate_min_wall_check(&terminal, min_mm)
                    }
                };
                let mut check = VerificationCheck::new(
                    "L2_MIN_WALL",
                    VerificationLayer::L2InterfacesConstraints,
                    status,
                    format!("assertion `{assertion_id}`: {message}"),
                )
                .with_evidence("min_wall_mm", min_mm);
                if let Some(target_id) = target {
                    check = check.with_evidence("target", target_id.0.clone());
                }
                if let Some(summary) = summary {
                    if let Some(measured) = summary.min_measured_wall_mm {
                        check = check.with_evidence("min_measured_wall_mm", measured);
                    }
                    if !summary.repair_hints.is_empty() {
                        check = check.with_evidence(
                            "repair_hints",
                            serde_json::json!(summary.repair_hints),
                        );
                    }
                }
                check
            }
            None => VerificationCheck::new(
                "L2_MIN_WALL",
                VerificationLayer::L2InterfacesConstraints,
                TriVerdict::Unknown,
                format!("assertion `{assertion_id}` wall thickness is not a resolved length"),
            ),
        },
        DesignAssertionKind::BooleanCondition { condition } => {
            evaluate_boolean_condition(view, assertion_id, condition, visited)
        }
        DesignAssertionKind::Custom { .. } => VerificationCheck::new(
            "L2_CUSTOM",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Unknown,
            format!("assertion `{assertion_id}` is an external expression and was not executed"),
        ),
    }
}

fn evaluate_boolean_condition(
    view: &VerifyView<'_>,
    assertion_id: &str,
    condition: &NodeId,
    visited: &mut BTreeSet<String>,
) -> VerificationCheck {
    let Some(node) = view.graph.nodes.get(&condition.0) else {
        return VerificationCheck::new(
            "L2_BOOLEAN",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Fail,
            format!("assertion `{assertion_id}` condition `{}` is missing", condition.0),
        );
    };
    let eval_state = view.eval_facts.node_states.get(&condition.0);
    if matches!(node.state, NodeState::Suppressed)
        || matches!(eval_state, Some(NodeState::Suppressed))
    {
        return VerificationCheck::new(
            "L2_BOOLEAN",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Unknown,
            format!(
                "assertion `{assertion_id}` condition `{}` is suppressed",
                condition.0
            ),
        );
    }
    if view.eval_facts.failed_ids.contains(&condition.0)
        || matches!(node.state, NodeState::FailedWithRollback { .. })
        || matches!(eval_state, Some(NodeState::FailedWithRollback { .. }))
    {
        return VerificationCheck::new(
            "L2_BOOLEAN",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Fail,
            format!(
                "assertion `{assertion_id}` condition `{}` failed evaluation",
                condition.0
            ),
        );
    }

    match &node.kind {
        NodeKind::ParameterRef { param_id } => {
            let resolved = resolve_formula_values(view.graph)
                .ok()
                .and_then(|(_, values)| values.get(param_id).cloned())
                .or_else(|| view.graph.parameters.get(&param_id.0).map(|def| def.value.clone()));
            match resolved {
                Some(IrValue::Bool(true)) => VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    TriVerdict::Pass,
                    format!(
                        "assertion `{assertion_id}` condition `{}` evaluated to true",
                        condition.0
                    ),
                )
                .with_evidence("condition_value", true),
                Some(IrValue::Bool(false)) => VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    TriVerdict::Fail,
                    format!(
                        "assertion `{assertion_id}` condition `{}` evaluated to false",
                        condition.0
                    ),
                )
                .with_evidence("condition_value", false),
                _ => VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    TriVerdict::Unknown,
                    format!(
                        "assertion `{assertion_id}` condition `{}` does not resolve to a boolean value",
                        condition.0
                    ),
                ),
            }
        }
        NodeKind::AssertGate => {
            if !visited.insert(condition.0.clone()) {
                return VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    TriVerdict::Unknown,
                    format!(
                        "assertion `{assertion_id}` condition `{}` forms an assertion cycle",
                        condition.0
                    ),
                );
            }
            if let Ok(FeatureSpec::DesignAssertion {
                target,
                assertion_id: nested_id,
                kind: nested_kind,
            }) = node.try_feature_spec()
            {
                if is_design_contract(&node.id.0, &nested_kind) {
                    return VerificationCheck::new(
                        "L2_BOOLEAN",
                        VerificationLayer::L2InterfacesConstraints,
                        TriVerdict::Unknown,
                        format!(
                            "assertion `{assertion_id}` condition `{}` references an unverified design contract",
                            condition.0
                        ),
                    );
                }
                let nested = assertion_check(view, target.as_ref(), &nested_id, &nested_kind, visited);
                return VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    nested.status,
                    format!(
                        "assertion `{assertion_id}` condition `{}`: {}",
                        condition.0, nested.message
                    ),
                );
            }
            if let Some(IrValue::Bool(flag)) = node.inputs.get("condition") {
                return VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    if *flag {
                        TriVerdict::Pass
                    } else {
                        TriVerdict::Fail
                    },
                    format!(
                        "assertion `{assertion_id}` condition `{}` evaluated to {flag}",
                        condition.0
                    ),
                )
                .with_evidence("condition_value", *flag);
            }
            match eval_state {
                Some(NodeState::Valid) => VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    TriVerdict::Pass,
                    format!(
                        "assertion `{assertion_id}` assert gate `{}` passed evaluation",
                        condition.0
                    ),
                ),
                _ => VerificationCheck::new(
                    "L2_BOOLEAN",
                    VerificationLayer::L2InterfacesConstraints,
                    TriVerdict::Unknown,
                    format!(
                        "assertion `{assertion_id}` condition `{}` has no evaluated boolean output",
                        condition.0
                    ),
                ),
            }
        }
        _ => VerificationCheck::new(
            "L2_BOOLEAN",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Unknown,
            format!(
                "assertion `{assertion_id}` condition `{}` does not produce a boolean output",
                condition.0
            ),
        ),
    }
}

fn sketch_check(sketch: &SketchFact) -> VerificationCheck {
    let failed = matches!(
        sketch.status,
        SolveStatus::OverConstrained | SolveStatus::FailedToConverge
    ) || sketch.remaining_dof < 0;
    if failed {
        VerificationCheck::new(
            "L2_SKETCH",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Fail,
            format!(
                "sketch `{}` status {:?} remaining_dof {}",
                sketch.node_id, sketch.status, sketch.remaining_dof
            ),
        )
    } else {
        VerificationCheck::new(
            "L2_SKETCH",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Pass,
            format!(
                "sketch `{}` converged with remaining_dof {}",
                sketch.node_id, sketch.remaining_dof
            ),
        )
        .with_evidence("remaining_dof", sketch.remaining_dof)
    }
}

fn mate_check(view: &VerifyView<'_>) -> Option<VerificationCheck> {
    let has_report = view.eval_facts.mates_converged.is_some()
        || !view.eval_facts.conflicting_mates.is_empty()
        || view.eval_facts.unsatisfied_mates > 0;
    if !has_report {
        return None;
    }
    let converged = view.eval_facts.mates_converged.unwrap_or(true);
    if converged
        && view.eval_facts.conflicting_mates.is_empty()
        && view.eval_facts.unsatisfied_mates == 0
    {
        Some(VerificationCheck::new(
            "L2_MATES",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Pass,
            "assembly mates converged",
        ))
    } else {
        Some(VerificationCheck::new(
            "L2_MATES",
            VerificationLayer::L2InterfacesConstraints,
            TriVerdict::Fail,
            format!(
                "mates did not converge; conflicts [{}]; unsatisfied {}",
                view.eval_facts.conflicting_mates.join(", "),
                view.eval_facts.unsatisfied_mates
            ),
        ))
    }
}

fn length_mm(graph: &CanonicalGraph, dim: &DimInput) -> Option<f64> {
    match dim {
        DimInput::Literal(quantity) => Some(quantity.canonical_value()),
        DimInput::Param { param_id } => {
            if let Ok((_, values)) = resolve_formula_values(graph)
                && let Some(IrValue::Quantity(quantity)) = values.get(param_id)
            {
                return Some(quantity.canonical_value());
            }
            match graph.parameters.get(&param_id.0).map(|def| &def.value) {
                Some(IrValue::Quantity(quantity)) => Some(quantity.canonical_value()),
                Some(IrValue::Scalar(value)) if value.is_finite() => Some(*value),
                _ => None,
            }
        }
        DimInput::NodeOutput { .. } => None,
    }
}

fn primary_volume(
    graph: &CanonicalGraph,
    kernel: &dyn GeometryKernel,
    solids: &BTreeMap<NodeId, SolidShape>,
) -> Option<f64> {
    let selected = select_solids(graph, solids, None, None).ok()?;
    let (_, solid) = primary_solid(graph, kernel, &selected).ok()?;
    let volume = kernel.check_solid(solid).volume_mm3;
    (volume.is_finite() && volume > 0.0).then_some(volume)
}

fn manufacturing_checks(view: &VerifyView<'_>) -> Vec<VerificationCheck> {
    let terminal = select_solids(view.graph, view.output_solids, None, None).unwrap_or_default();
    crate::l3_dfm::evaluate_l3_checks(view.graph, &terminal, view.eval_facts.fea_declared)
}

fn roundtrip_check(view: &VerifyView<'_>) -> VerificationCheck {
    if !view.kernel_identity.authoritative {
        return VerificationCheck::new(
            "L4_ANALYTIC",
            VerificationLayer::L4InteropRoundtrip,
            TriVerdict::Unknown,
            "analytic fallback does not run a STEP AP242 re-import",
        );
    }
    let Ok(solids) = select_solids(view.graph, view.output_solids, None, None) else {
        return VerificationCheck::new(
            "L4_NO_SOLID",
            VerificationLayer::L4InteropRoundtrip,
            TriVerdict::Unknown,
            "no evaluated solid for a STEP round-trip",
        );
    };
    let kernel = view.kernel;
    let Ok((_, solid)) = primary_solid(view.graph, kernel, &solids) else {
        return VerificationCheck::new(
            "L4_NO_SOLID",
            VerificationLayer::L4InteropRoundtrip,
            TriVerdict::Unknown,
            "no evaluated solid for a STEP round-trip",
        );
    };
    let before = kernel.check_solid(solid).volume_mm3;
    let exported = match kernel.export_step(solid) {
        Ok(text) => text,
        Err(err) => {
            return VerificationCheck::new(
                "L4_ROUNDTRIP",
                VerificationLayer::L4InteropRoundtrip,
                TriVerdict::Fail,
                format!("STEP export failed: {err}"),
            );
        }
    };
    let Some(span) = solid
        .faces
        .first()
        .map(|face| face.ast_span)
        .or_else(|| solid.edges.first().map(|edge| edge.ast_span))
    else {
        return VerificationCheck::new(
            "L4_ROUNDTRIP",
            VerificationLayer::L4InteropRoundtrip,
            TriVerdict::Unknown,
            "evaluated solid has no source span for STEP re-import",
        );
    };
    match import_occt_bytes(
        exported.as_bytes(),
        OcctImportFormat::Step,
        &OcctImportOptions::default(),
        None,
        false,
        span,
    ) {
        Err(KernelError::BackendUnavailable) => VerificationCheck::new(
            "L4_ANALYTIC",
            VerificationLayer::L4InteropRoundtrip,
            TriVerdict::Unknown,
            "OCCT import is unavailable in this build",
        ),
        Err(err) => VerificationCheck::new(
            "L4_ROUNDTRIP",
            VerificationLayer::L4InteropRoundtrip,
            TriVerdict::Fail,
            format!("STEP re-import failed: {err}"),
        ),
        Ok((imported, _)) => {
            let after = kernel.check_solid(&imported).volume_mm3;
            let tolerance = (before.abs() * 0.001).max(1.0e-3);
            if before.is_finite() && after.is_finite() && (before - after).abs() <= tolerance {
                VerificationCheck::new(
                    "L4_ROUNDTRIP",
                    VerificationLayer::L4InteropRoundtrip,
                    TriVerdict::Pass,
                    "STEP AP242 re-import volume matches",
                )
                .with_evidence("volume_mm3", before)
                .with_evidence("imported_volume_mm3", after)
            } else {
                VerificationCheck::new(
                    "L4_ROUNDTRIP",
                    VerificationLayer::L4InteropRoundtrip,
                    TriVerdict::Fail,
                    format!("STEP round-trip volume {before} mm^3 became {after} mm^3"),
                )
            }
        }
    }
}

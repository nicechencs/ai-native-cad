//! Commands for creating and exchanging hybrid `.aicad` project containers.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use cad_container::{
    AicadProject, BranchPurpose, CacheStatus, ContainerManifest, KclProjectionPolicy,
    KernelFingerprint, LazyProjectReader, SqliteContainer, StorageKind, apply_patch_in_place,
    detect_storage_kind, pack_and_go, vacuum_container,
};
use cad_evaluator::GraphEvaluator;
use cad_implicit::{
    BoxSdf, ConstantField, CylinderSdf, LatticeInfillSdf, ScalarField, TpmsField, TpmsSheetField,
};
use cad_ir::{
    CanonicalGraph, ContextSliceOptions, GraphPatch, IrNode, IrType, IrValue, LengthUnit, NodeId,
    NodeKind, NodeState, OUTPUT_PORT, ParamId, ParameterDef, Quantity, extract_context_slice,
};
use cad_kcl_bridge::emit_kcl;
use cad_kernel_occt::GeometryKernel;
use cad_mesh_io::{Mesh, write_3mf_mesh_file};
use cad_render::{DimensionStatus, render_associative_sheet_svg, resolve_drawing_sheet};
use cad_slicer::FnSliceField;
use cad_slicer::{SliceConfig, export_cli_ascii, export_layer_svg, slice_field};
use cad_topo::MatchTolerances;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Args)]
pub(crate) struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Create a project container with a starter box.
    Init {
        dir: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value_t = InitFormat::Directory)]
        format: InitFormat,
    },
    /// Pack a project container into a single-file SQLite container.
    Pack {
        src_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value_t = PackFormat::Sqlite)]
        format: PackFormat,
    },
    /// Unpack a SQLite container into a standard or sharded project directory.
    Unpack {
        archive: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        sharded: bool,
    },
    /// Inspect any `.aicad` container (directory, sharded directory, or SQLite).
    Inspect {
        path: PathBuf,
        /// Read metadata and graph headers lazily without streaming or hashing binary blobs.
        #[arg(long = "metadata-only")]
        metadata_only: bool,
    },
    /// Emit the canonical `.kcl 2.0` projection.
    EmitKcl {
        path: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Apply an atomic graph patch (in-place ACID transaction for SQLite containers).
    ApplyPatch {
        path: PathBuf,
        #[arg(long)]
        patch: PathBuf,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Manage lightweight branches inside a SQLite `.aicad` container.
    Branch {
        path: PathBuf,
        #[command(subcommand)]
        action: BranchAction,
    },
    #[command(hide = true)]
    Checkout {
        path: PathBuf,
        branch_name: String,
    },
    /// Extract a closed local subgraph around focus nodes and project it to `.kcl 2.0`.
    #[command(name = "slice-context")]
    SliceContext {
        path: PathBuf,
        #[arg(long = "focus", required = true, num_args = 1.., value_delimiter = ',')]
        focus: Vec<String>,
        #[arg(long = "up", alias = "upstream-depth", default_value_t = 1)]
        up: usize,
        #[arg(long = "down", alias = "downstream-depth", default_value_t = 1)]
        down: usize,
        #[arg(long = "no-closure")]
        no_closure: bool,
        #[arg(long = "out-graph")]
        out_graph: Option<PathBuf>,
        #[arg(long = "out-kcl")]
        out_kcl: Option<PathBuf>,
    },
    /// Sweep orphan blobs, stale node caches, and discarded branches, then compact SQLite pages.
    Vacuum {
        path: PathBuf,
        #[arg(long = "keep-recent-oplog")]
        keep_recent_oplog: Option<usize>,
    },
    /// Bundle a project and all transitive `ExternalRef` containers into one standalone SQLite file.
    #[command(name = "pack-and-go")]
    PackAndGo {
        path: PathBuf,
        #[arg(long = "workspace-root")]
        workspace_root: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Merge STEP XCAF appearances, BOM, and PMI into a project.
    ImportStepXcaf {
        path: PathBuf,
        /// Read XCAF metadata from a STEP file using OCCT.
        #[arg(long, value_name = "FILE")]
        step: Option<PathBuf>,
        /// Read XCAF metadata from JSON.
        #[arg(long, value_name = "FILE")]
        xcaf_json: Option<PathBuf>,
        /// Mount the STEP file bytes as a content-addressed ImportAsset node in the project graph.
        #[arg(long)]
        mount_geometry: bool,
        /// Replace `project.graph.outputs` with the newly mounted ImportAsset node (requires --mount-geometry).
        #[arg(long)]
        replace_outputs: bool,
    },
    /// Write a STEP AP242 assembly from an XCAF export specification.
    ExportStepXcaf {
        #[arg(long, value_name = "FILE")]
        spec: PathBuf,
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
    },
    /// Query the engineering standard library.
    Stdlib {
        kind: StdlibKind,
        query: String,
        /// Nominal size in millimeters, required for ISO 286.
        #[arg(long)]
        nominal_mm: Option<f64>,
    },
    /// Slice an implicit manufacturing field into a CLI contour stack.
    Slice {
        path: PathBuf,
        #[arg(long = "out-cli")]
        out_cli: PathBuf,
        #[arg(long = "out-svg")]
        out_svg: Option<PathBuf>,
        #[arg(long, default_value_t = 1.0)]
        layer_height: f64,
        #[arg(long, default_value_t = 0.5)]
        xy_step: f64,
    },
    /// Resolve an associative drawing sheet and render it to SVG.
    RenderDrawing {
        path: PathBuf,
        #[arg(long)]
        sheet: String,
        #[arg(long = "out-svg")]
        out_svg: PathBuf,
    },
    /// Export the primary evaluated project mesh as a 3MF archive.
    #[command(name = "export-3mf")]
    Export3mf {
        path: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// TPMS sampling grid cells per axis.
        #[arg(long)]
        resolution: Option<usize>,
        #[arg(from_global)]
        json: bool,
    },
    /// Export evaluated project meshes and an assembly index as a cadpack.
    ExportCadpack {
        path: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Page through or search a cadpack assembly tree.
    CadpackTree {
        path: PathBuf,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        query: Option<String>,
    },
    /// Summarize cadpack components intersecting a bounding box in millimeters.
    CadpackDigest {
        path: PathBuf,
        #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true, value_parser = crate::cadpack_cmd::parse_vec3)]
        min: [f64; 3],
        #[arg(long, value_name = "X,Y,Z", allow_hyphen_values = true, value_parser = crate::cadpack_cmd::parse_vec3)]
        max: [f64; 3],
    },
    /// Check project meshes for interference, thin walls, and draft angles.
    DfmCheck {
        path: PathBuf,
        #[arg(long, default_value_t = 1.5)]
        min_wall_mm: f64,
        #[arg(long, default_value_t = 1.0, allow_negative_numbers = true)]
        min_draft_deg: f64,
        #[arg(long, default_value = "0,0,1", value_name = "X,Y,Z", allow_hyphen_values = true, value_parser = crate::cadpack_cmd::parse_vec3)]
        pull_dir: [f64; 3],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum InitFormat {
    #[value(name = "directory", alias = "single-dir", alias = "single_dir")]
    Directory,
    #[value(
        name = "sharded-dir",
        alias = "sharded_directory",
        alias = "sharded_dir",
        alias = "sharded"
    )]
    ShardedDir,
    #[value(name = "sqlite")]
    Sqlite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum PackFormat {
    #[value(name = "sqlite")]
    Sqlite,
}

#[derive(Subcommand)]
enum BranchAction {
    /// List all branches in the SQLite container.
    List,
    /// Create a lightweight branch forked from an existing branch.
    Create {
        branch_name: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long, value_enum)]
        purpose: Option<CliBranchPurpose>,
    },
    /// Switch the active branch of the SQLite container.
    Checkout { branch_name: String },
    /// Roll a branch back to an earlier oplog sequence number.
    Rollback {
        #[arg(long = "to-seq", allow_hyphen_values = true)]
        to_seq: i64,
        #[arg(long)]
        branch: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CliBranchPurpose {
    #[value(name = "user")]
    User,
    #[value(name = "ai_candidate", alias = "ai-candidate")]
    AiCandidate,
}

impl From<CliBranchPurpose> for BranchPurpose {
    fn from(value: CliBranchPurpose) -> Self {
        match value {
            CliBranchPurpose::User => Self::User,
            CliBranchPurpose::AiCandidate => Self::AiCandidate,
        }
    }
}

#[derive(Clone, ValueEnum)]
enum StdlibKind {
    Cbore,
    Csink,
    Tap,
    Bearing,
    Material,
    Iso286,
}

pub(super) enum CommandOutput {
    Kcl(String),
    Report { text: String, json: Value },
}

impl CommandOutput {
    pub(super) fn report(text: String, json: Value) -> Self {
        Self::Report { text, json }
    }
}

pub(crate) fn run(args: ProjectArgs, kernel: &dyn GeometryKernel) -> ExitCode {
    let json_requested =
        args.json || matches!(&args.command, ProjectCommand::Export3mf { json: true, .. });
    match dispatch(args.command, kernel, json_requested) {
        Ok(status) => status,
        Err(error) => emit_error(error, json_requested),
    }
}

pub(super) fn emit_output(output: CommandOutput, json_requested: bool) -> ExitCode {
    match output {
        CommandOutput::Kcl(kcl) if json_requested => {
            println!("{:#}", json!({ "status": "ok", "kcl": kcl }));
        }
        CommandOutput::Kcl(kcl) => print!("{kcl}"),
        CommandOutput::Report { json, .. } if json_requested => println!("{json:#}"),
        CommandOutput::Report { text, .. } => println!("{text}"),
    }
    ExitCode::SUCCESS
}

pub(super) fn emit_error(error: impl std::fmt::Display, json_requested: bool) -> ExitCode {
    if json_requested {
        println!(
            "{:#}",
            json!({ "status": "error", "message": format!("{error:#}") })
        );
    }
    eprintln!("error: {error:#}");
    tracing::error!(code = cad_observe::codes::R016_IO_ERROR, error_type = "IoError", "{}", format!("{error:#}"));
    ExitCode::from(2)
}

fn dispatch(
    command: ProjectCommand,
    kernel: &dyn GeometryKernel,
    json_requested: bool,
) -> Result<ExitCode> {
    let output = match command {
        ProjectCommand::Init { dir, name, format } => init_project(&dir, &name, format, kernel),
        ProjectCommand::Pack {
            src_dir,
            out,
            format,
        } => pack_project(&src_dir, &out, format),
        ProjectCommand::Unpack {
            archive,
            out,
            sharded,
        } => unpack_project(&archive, &out, sharded),
        ProjectCommand::Inspect {
            path,
            metadata_only,
        } => inspect_project(&path, metadata_only, kernel),
        ProjectCommand::EmitKcl { path, out } => emit_project_kcl(&path, out.as_deref()),
        ProjectCommand::ApplyPatch {
            path,
            patch,
            branch,
        } => apply_project_patch(&path, &patch, branch.as_deref()),
        ProjectCommand::Branch { path, action } => branch_project(&path, action),
        ProjectCommand::Checkout { path, branch_name } => {
            branch_project(&path, BranchAction::Checkout { branch_name })
        }
        ProjectCommand::SliceContext {
            path,
            focus,
            up,
            down,
            no_closure,
            out_graph,
            out_kcl,
        } => slice_context_project(
            &path,
            &focus,
            up,
            down,
            !no_closure,
            out_graph.as_deref(),
            out_kcl.as_deref(),
        ),
        ProjectCommand::Vacuum {
            path,
            keep_recent_oplog,
        } => vacuum_project(&path, keep_recent_oplog),
        ProjectCommand::PackAndGo {
            path,
            workspace_root,
            out,
        } => pack_and_go_project(&path, &workspace_root, &out),
        ProjectCommand::ImportStepXcaf {
            path,
            step,
            xcaf_json,
            mount_geometry,
            replace_outputs,
        } => {
            return crate::xcaf_cmd::run_import_step_xcaf(
                &path,
                step.as_deref(),
                xcaf_json.as_deref(),
                mount_geometry,
                replace_outputs,
                json_requested,
            );
        }
        ProjectCommand::ExportStepXcaf { spec, out } => {
            return crate::xcaf_cmd::run_export_step_xcaf(&spec, &out, json_requested);
        }
        ProjectCommand::Stdlib {
            kind,
            query,
            nominal_mm,
        } => stdlib_lookup(kind, &query, nominal_mm),
        ProjectCommand::Slice {
            path,
            out_cli,
            out_svg,
            layer_height,
            xy_step,
        } => slice_project(&path, &out_cli, out_svg.as_deref(), layer_height, xy_step),
        ProjectCommand::RenderDrawing {
            path,
            sheet,
            out_svg,
        } => render_drawing_project(&path, &sheet, &out_svg, kernel),
        ProjectCommand::Export3mf {
            path,
            out,
            resolution,
            json: _,
        } => export_3mf_project(&path, &out, resolution, kernel),
        ProjectCommand::ExportCadpack { path, out } => {
            crate::cadpack_cmd::export(&path, &out, kernel)
        }
        ProjectCommand::CadpackTree {
            path,
            offset,
            limit,
            query,
        } => crate::cadpack_cmd::tree(&path, offset, limit, query.as_deref()),
        ProjectCommand::CadpackDigest { path, min, max } => {
            crate::cadpack_cmd::digest(&path, min, max)
        }
        ProjectCommand::DfmCheck {
            path,
            min_wall_mm,
            min_draft_deg,
            pull_dir,
        } => crate::cadpack_cmd::dfm_check(&path, min_wall_mm, min_draft_deg, pull_dir, kernel),
    }?;
    Ok(emit_output(output, json_requested))
}

fn current_kernel(kernel: &dyn GeometryKernel) -> KernelFingerprint {
    KernelFingerprint::new(kernel.backend_name(), env!("CARGO_PKG_VERSION"))
}

pub(super) fn storage_kind_cli_name(kind: StorageKind) -> &'static str {
    match kind {
        StorageKind::UnpackedSingleGraphDir => "directory",
        StorageKind::UnpackedShardedDir => "sharded_directory",
        StorageKind::SqliteContainer => "sqlite",
    }
}

pub(super) fn load_project_auto(path: &Path) -> Result<(ProjectFormat, AicadProject)> {
    let format = ProjectFormat::detect(path)?;
    let project = format.load(path)?;
    Ok((format, project))
}

fn init_project(
    target: &Path,
    name: &str,
    format: InitFormat,
    kernel: &dyn GeometryKernel,
) -> Result<CommandOutput> {
    if name.trim().is_empty() {
        bail!("project name must not be empty");
    }

    match format {
        InitFormat::Directory | InitFormat::ShardedDir => {
            let existing_entry = match fs::read_dir(target) {
                Ok(mut entries) => entries.next().transpose(),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
            .with_context(|| format!("cannot read project directory `{}`", target.display()))?;
            if existing_entry.is_some() {
                bail!("project directory `{}` is not empty", target.display());
            }
        }
        InitFormat::Sqlite => {
            if fs::symlink_metadata(target).is_ok() {
                bail!("project target `{}` already exists", target.display());
            }
        }
    }

    let graph = starter_graph()?;
    let mut manifest = ContainerManifest::new(name, current_kernel(kernel));
    if matches!(format, InitFormat::ShardedDir | InitFormat::Sqlite) {
        manifest.kcl_projection_policy = KclProjectionPolicy::OnDemandDynamic;
        manifest.kcl_view = None;
    }

    let mut project = AicadProject::new(manifest, graph);
    if project.manifest.kcl_projection_policy == KclProjectionPolicy::MaterializedSynced {
        project.kcl_view = Some(emit_kcl(&project.graph).context("cannot emit starter KCL")?);
    }

    let target_format = match format {
        InitFormat::Directory => ProjectFormat::Directory,
        InitFormat::ShardedDir => ProjectFormat::ShardedDirectory,
        InitFormat::Sqlite => ProjectFormat::Sqlite,
    };
    target_format.save(&project, target)?;

    Ok(CommandOutput::report(
        format!("Created project {name} at {}", target.display()),
        json!({
            "status": "ok",
            "project_name": name,
            "path": target.to_string_lossy(),
            "storage": target_format.name(),
        }),
    ))
}

fn starter_graph() -> Result<CanonicalGraph> {
    let mut graph = CanonicalGraph::new("main");
    let mut inputs_for_box = BTreeMap::new();
    for (name, value) in [("width", 20.0), ("height", 10.0), ("depth", 15.0)] {
        let value = Quantity::length(value, LengthUnit::Mm)?;
        graph.parameters.insert(
            name.to_owned(),
            ParameterDef {
                name: ParamId::from(name),
                ir_type: IrType::Length,
                value: IrValue::Quantity(value),
                formula: None,
            },
        );
        let id = format!("{name}_param");
        graph.nodes.insert(
            id.clone(),
            IrNode {
                id: NodeId::from(id.clone()),
                name: id.clone(),
                kind: NodeKind::ParameterRef {
                    param_id: ParamId::from(name),
                },
                inputs: BTreeMap::new(),
                output_type: IrType::Length,
                state: NodeState::Valid,
                kernel_op_version: None,
                metadata: BTreeMap::new(),
            },
        );
        inputs_for_box.insert(
            name.to_owned(),
            IrValue::NodeRef {
                node_id: NodeId::from(id),
                port: OUTPUT_PORT.to_owned(),
            },
        );
    }
    inputs_for_box.insert("origin".to_owned(), IrValue::Vec3([0.0, 0.0, 0.0]));
    graph.nodes.insert(
        "box".to_owned(),
        IrNode {
            id: NodeId::from("box"),
            name: "Starter box".to_owned(),
            kind: NodeKind::PrimitiveBox,
            inputs: inputs_for_box,
            output_type: IrType::Solid,
            state: NodeState::Valid,
            kernel_op_version: None,
            metadata: BTreeMap::new(),
        },
    );
    graph.outputs.push(NodeId::from("box"));
    Ok(graph)
}

fn pack_project(src: &Path, out: &Path, format: PackFormat) -> Result<CommandOutput> {
    let (_, project) = load_project_auto(src)?;

    let format_name = match format {
        PackFormat::Sqlite => {
            project.save_sqlite(out).with_context(|| {
                format!("cannot save SQLite project container `{}`", out.display())
            })?;
            "sqlite"
        }
    };

    Ok(CommandOutput::report(
        format!("Packed {} to {} ({format_name})", src.display(), out.display()),
        json!({
            "status": "ok",
            "source": src.to_string_lossy(),
            "archive": out.to_string_lossy(),
            "out": out.to_string_lossy(),
            "format": format_name,
        }),
    ))
}

fn unpack_project(src: &Path, out: &Path, sharded: bool) -> Result<CommandOutput> {
    let (_, mut project) = load_project_auto(src)?;
    let target_format = if sharded {
        ProjectFormat::ShardedDirectory
    } else {
        if project.manifest.kcl_projection_policy == KclProjectionPolicy::MaterializedSynced
            && project.kcl_view.is_none()
        {
            project.kcl_view = Some(emit_kcl(&project.graph).context("cannot project KCL view")?);
        }
        ProjectFormat::Directory
    };
    target_format.save(&project, out)?;
    Ok(CommandOutput::report(
        format!("Unpacked {} to {}", src.display(), out.display()),
        json!({
            "status": "ok",
            "archive": src.to_string_lossy(),
            "path": out.to_string_lossy(),
            "sharded": sharded,
            "storage": target_format.name(),
        }),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectFormat {
    Directory,
    ShardedDirectory,
    Sqlite,
}

impl ProjectFormat {
    pub(super) fn detect(path: &Path) -> Result<Self> {
        let kind = detect_storage_kind(path)
            .with_context(|| format!("cannot access project `{}`", path.display()))?;
        Ok(match kind {
            StorageKind::UnpackedSingleGraphDir => Self::Directory,
            StorageKind::UnpackedShardedDir => Self::ShardedDirectory,
            StorageKind::SqliteContainer => Self::Sqlite,
        })
    }

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::ShardedDirectory => "sharded_directory",
            Self::Sqlite => "sqlite",
        }
    }

    pub(super) fn load(self, path: &Path) -> Result<AicadProject> {
        match self {
            Self::Directory => AicadProject::load_directory(path),
            Self::ShardedDirectory => AicadProject::load_sharded_directory(path),
            Self::Sqlite => AicadProject::load_sqlite(path),
        }
        .with_context(|| format!("cannot load project {} `{}`", self.name(), path.display()))
    }

    pub(super) fn save(self, project: &AicadProject, path: &Path) -> Result<()> {
        match self {
            Self::Directory => project.save_directory(path),
            Self::ShardedDirectory => project.save_sharded_directory(path),
            Self::Sqlite => project.save_sqlite(path),
        }
        .with_context(|| format!("cannot save project {} `{}`", self.name(), path.display()))
    }
}

fn inspect_project(
    path: &Path,
    metadata_only: bool,
    kernel: &dyn GeometryKernel,
) -> Result<CommandOutput> {
    let reader = LazyProjectReader::open(path)
        .with_context(|| format!("cannot inspect project `{}`", path.display()))?;
    if !metadata_only {
        for blob in reader.graph().blobs.values() {
            reader
                .stream_blob(&blob.relative_path, &mut std::io::sink())
                .with_context(|| {
                    format!(
                        "blob integrity check failed for `{}` in `{}`",
                        blob.relative_path,
                        path.display()
                    )
                })?;
        }
    }

    let summary = reader.inspect_summary(&current_kernel(kernel));
    let cache_status = cache_status_name(summary.cache_status);
    let storage = storage_kind_cli_name(summary.storage_kind);
    let project_name = &summary.project_name;
    let format_version = summary.format_version;
    let ir_schema_version = summary.ir_schema_version;
    let node_count = summary.node_count;
    let parameter_count = summary.parameter_count;
    let blob_count = summary.blob_count;
    let pmi_count = summary.pmi_count;
    let bom_count = summary.bom_count;
    let drawing_count = summary.drawing_count;

    Ok(CommandOutput::report(
        format!(
            "{project_name}\n\
             Format {format_version}, IR schema {ir_schema_version}, storage {storage}, branch {}\n\
             Nodes {node_count}, parameters {parameter_count}, blobs {blob_count}, extrefs {}\n\
             PMI {pmi_count}, BOM {bom_count}, drawings {drawing_count}\n\
             Cache {cache_status} (node caches: {}, bundled refs: {})",
            summary.active_branch,
            summary.external_ref_count,
            summary.node_brep_cache_count,
            summary.bundled_ref_count,
        ),
        json!({
            "status": "ok",
            "project_name": project_name,
            "format_version": format_version,
            "ir_schema_version": ir_schema_version,
            "active_branch": summary.active_branch,
            "node_count": node_count,
            "parameter_count": parameter_count,
            "blob_count": blob_count,
            "external_ref_count": summary.external_ref_count,
            "pmi_count": pmi_count,
            "appearance_count": summary.appearance_count,
            "bom_count": bom_count,
            "drawing_count": drawing_count,
            "node_brep_cache_count": summary.node_brep_cache_count,
            "bundled_ref_count": summary.bundled_ref_count,
            "has_cadpack": summary.has_cadpack,
            "cache_status": cache_status,
            "storage": storage,
        }),
    ))
}

fn cache_status_name(status: CacheStatus) -> &'static str {
    match status {
        CacheStatus::FreshExactMatch => "fresh_exact_match",
        CacheStatus::KernelVersionDrift { .. } => "kernel_version_drift",
        CacheStatus::GraphModified => "graph_modified",
        CacheStatus::Missing => "missing",
    }
}

fn emit_project_kcl(path: &Path, out: Option<&Path>) -> Result<CommandOutput> {
    let reader = LazyProjectReader::open(path)
        .with_context(|| format!("cannot open project `{}`", path.display()))?;
    let kcl = emit_kcl(reader.graph()).context("cannot emit KCL")?;
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(out, kcl).with_context(|| format!("cannot write KCL to `{}`", out.display()))?;
        Ok(CommandOutput::report(
            format!("Wrote KCL to {}", out.display()),
            json!({ "status": "ok", "path": out.to_string_lossy() }),
        ))
    } else {
        Ok(CommandOutput::Kcl(kcl))
    }
}

fn apply_project_patch(
    path: &Path,
    patch_path: &Path,
    branch: Option<&str>,
) -> Result<CommandOutput> {
    let patch_text = fs::read_to_string(patch_path)
        .with_context(|| format!("cannot read patch `{}`", patch_path.display()))?;
    let patch: GraphPatch = serde_json::from_str(&patch_text)
        .with_context(|| format!("cannot parse patch `{}`", patch_path.display()))?;

    let storage = ProjectFormat::detect(path)?;
    if storage == ProjectFormat::Sqlite {
        let entry = apply_patch_in_place(path, &patch, branch)
            .with_context(|| format!("cannot apply patch `{}`", patch.patch_id))?;
        return Ok(CommandOutput::report(
            format!(
                "Applied patch {} to {} (branch {}, seq {})",
                entry.patch_id,
                path.display(),
                entry.branch_name,
                entry.seq
            ),
            json!({
                "status": "ok",
                "patch_id": entry.patch_id,
                "path": path.to_string_lossy(),
                "branch": entry.branch_name,
                "branch_name": entry.branch_name,
                "seq": entry.seq,
                "oplog_seq": entry.seq,
                "outputs_merkle": entry.outputs_merkle,
            }),
        ));
    }

    if let Some(branch_name) = branch
        && branch_name != "main"
    {
        bail!(
            "--branch `{branch_name}` requires a SQLite `.aicad` container (storage `{}` supports only `main`)",
            storage.name()
        );
    }

    let mut project = storage.load(path)?;
    project
        .graph
        .apply_patch(&patch)
        .with_context(|| format!("cannot apply patch `{}`", patch.patch_id))?;
    match project.manifest.kcl_projection_policy {
        KclProjectionPolicy::MaterializedSynced => {
            project.kcl_view = Some(emit_kcl(&project.graph).context("cannot refresh KCL view")?);
        }
        KclProjectionPolicy::OnDemandDynamic => {
            project.kcl_view = None;
        }
    }
    storage.save(&project, path)?;
    Ok(CommandOutput::report(
        format!("Applied patch {} to {}", patch.patch_id, path.display()),
        json!({
            "status": "ok",
            "patch_id": patch.patch_id,
            "path": path.to_string_lossy(),
            "branch": "main",
            "branch_name": "main",
        }),
    ))
}

fn open_sqlite_for_cli(path: &Path) -> Result<SqliteContainer> {
    let kind = detect_storage_kind(path)
        .with_context(|| format!("cannot access container `{}`", path.display()))?;
    if kind != StorageKind::SqliteContainer {
        bail!(
            "operation requires a SQLite `.aicad` container at `{}` (found `{}`)",
            path.display(),
            storage_kind_cli_name(kind)
        );
    }
    SqliteContainer::open(path)
        .with_context(|| format!("cannot open SQLite container `{}`", path.display()))
}

fn branch_project(path: &Path, action: BranchAction) -> Result<CommandOutput> {
    let mut container = open_sqlite_for_cli(path)?;
    match action {
        BranchAction::List => {
            let active_branch = container.active_branch()?;
            let branches = container.list_branches()?;
            let mut lines = Vec::with_capacity(branches.len());
            for info in &branches {
                let marker = if info.is_active { "*" } else { " " };
                lines.push(format!(
                    "{marker} {} (seq {}, purpose {}, status {})",
                    info.branch_name,
                    info.head_seq,
                    info.purpose.as_str(),
                    info.status.as_str()
                ));
            }
            Ok(CommandOutput::report(
                lines.join("\n"),
                json!({
                    "status": "ok",
                    "active_branch": active_branch,
                    "branches": branches,
                }),
            ))
        }
        BranchAction::Create {
            branch_name,
            from,
            purpose,
        } => {
            let info = match purpose {
                Some(purpose) => container
                    .create_branch_with_purpose(&branch_name, from.as_deref(), purpose.into())
                    .with_context(|| format!("cannot create branch `{branch_name}`"))?,
                None => container
                    .create_branch(&branch_name, from.as_deref())
                    .with_context(|| format!("cannot create branch `{branch_name}`"))?,
            };
            Ok(CommandOutput::report(
                format!(
                    "Created branch {} (seq {}, purpose {})",
                    info.branch_name,
                    info.head_seq,
                    info.purpose.as_str()
                ),
                json!({
                    "status": "ok",
                    "branch": info,
                }),
            ))
        }
        BranchAction::Checkout { branch_name } => {
            let graph = container
                .checkout_branch(&branch_name)
                .with_context(|| format!("cannot checkout branch `{branch_name}`"))?;
            let node_count = graph.nodes.len();
            Ok(CommandOutput::report(
                format!("Switched to branch {branch_name} ({node_count} nodes)"),
                json!({
                    "status": "ok",
                    "active_branch": branch_name,
                    "node_count": node_count,
                }),
            ))
        }
        BranchAction::Rollback { to_seq, branch } => {
            let (target_branch, graph) = match branch {
                Some(branch_name) => {
                    let g = container
                        .rollback_branch_to_seq(&branch_name, to_seq)
                        .with_context(|| {
                            format!("cannot roll back branch `{branch_name}` to seq {to_seq}")
                        })?;
                    (branch_name, g)
                }
                None => {
                    let active = container.active_branch()?;
                    let g = container.rollback_to_seq(to_seq).with_context(|| {
                        format!("cannot roll back active branch `{active}` to seq {to_seq}")
                    })?;
                    (active, g)
                }
            };
            let node_count = graph.nodes.len();
            Ok(CommandOutput::report(
                format!(
                    "Rolled back branch {target_branch} to seq {to_seq} ({node_count} nodes)"
                ),
                json!({
                    "status": "ok",
                    "active_branch": target_branch,
                    "head_seq": to_seq,
                    "node_count": node_count,
                }),
            ))
        }
    }
}

fn slice_context_project(
    path: &Path,
    raw_focus: &[String],
    upstream_depth: usize,
    downstream_depth: usize,
    ensure_valid_closure: bool,
    out_graph: Option<&Path>,
    out_kcl: Option<&Path>,
) -> Result<CommandOutput> {
    let mut focus_names = Vec::new();
    let mut focus_nodes = Vec::new();
    for raw in raw_focus {
        for item in raw.split(',') {
            let trimmed = item.trim();
            if !trimmed.is_empty() && !focus_names.iter().any(|existing| existing == trimmed) {
                focus_names.push(trimmed.to_owned());
                focus_nodes.push(NodeId::from(trimmed));
            }
        }
    }
    if focus_nodes.is_empty() {
        bail!("at least one non-empty --focus node ID is required");
    }

    let reader = LazyProjectReader::open(path)
        .with_context(|| format!("cannot open project `{}`", path.display()))?;
    let total_node_count = reader.graph().nodes.len();
    let options = ContextSliceOptions {
        focus_nodes,
        upstream_depth,
        downstream_depth,
        ensure_valid_closure,
    };
    let sliced = extract_context_slice(reader.graph(), &options)
        .with_context(|| format!("cannot slice context in `{}`", path.display()))?;

    let kcl = match emit_kcl(&sliced) {
        Ok(text) => text,
        Err(err) if !ensure_valid_closure => {
            format!("// unclosed slice cannot emit standalone KCL: {err}\n")
        }
        Err(err) => return Err(err).context("cannot emit KCL for context slice"),
    };

    if let Some(out_graph) = out_graph {
        if let Some(parent) = out_graph.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&sliced)?;
        fs::write(out_graph, bytes)
            .with_context(|| format!("cannot write sliced graph `{}`", out_graph.display()))?;
    }
    if let Some(out_kcl) = out_kcl {
        if let Some(parent) = out_kcl.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(out_kcl, &kcl)
            .with_context(|| format!("cannot write sliced KCL `{}`", out_kcl.display()))?;
    }

    let kept_node_count = sliced.nodes.len();
    Ok(CommandOutput::report(
        format!(
            "Sliced {kept_node_count} of {total_node_count} nodes around {:?}",
            focus_names
        ),
        json!({
            "status": "ok",
            "focus_nodes": focus_names,
            "kept_node_count": kept_node_count,
            "total_node_count": total_node_count,
            "graph": sliced,
            "kcl": kcl,
        }),
    ))
}

fn vacuum_project(path: &Path, keep_recent_oplog: Option<usize>) -> Result<CommandOutput> {
    let report = vacuum_container(path, keep_recent_oplog)
        .with_context(|| format!("cannot vacuum container `{}`", path.display()))?;
    Ok(CommandOutput::report(
        format!(
            "Vacuumed {} ({} -> {} bytes, reclaimed {} bytes, removed {} blobs, {} node caches, {} oplog entries)",
            path.display(),
            report.bytes_before,
            report.bytes_after,
            report.reclaimed_bytes,
            report.removed_blobs,
            report.removed_node_caches,
            report.pruned_oplog_entries
        ),
        json!({
            "status": "ok",
            "bytes_before": report.bytes_before,
            "bytes_after": report.bytes_after,
            "reclaimed_bytes": report.reclaimed_bytes,
            "removed_blobs": report.removed_blobs,
            "removed_node_caches": report.removed_node_caches,
            "pruned_oplog_entries": report.pruned_oplog_entries,
        }),
    ))
}

fn pack_and_go_project(path: &Path, workspace_root: &Path, out: &Path) -> Result<CommandOutput> {
    let report = pack_and_go(path, workspace_root, out)
        .with_context(|| format!("cannot pack-and-go `{}`", path.display()))?;
    Ok(CommandOutput::report(
        format!(
            "Pack-and-Go bundled {} external refs into {}",
            report.bundled_count,
            report.output_path.display()
        ),
        json!({
            "status": "ok",
            "output_path": report.output_path.to_string_lossy(),
            "bundled_count": report.bundled_count,
            "locked_hashes": report.locked_hashes,
        }),
    ))
}

fn stdlib_lookup(kind: StdlibKind, query: &str, nominal_mm: Option<f64>) -> Result<CommandOutput> {
    if nominal_mm.is_some() && !matches!(kind, StdlibKind::Iso286) {
        bail!("--nominal-mm applies only to the iso286 lookup");
    }
    let kind_name = kind_name(&kind);
    let result = match kind {
        StdlibKind::Cbore => serde_json::to_value(cad_stdlib::lookup_iso4762_cbore(query)?),
        StdlibKind::Csink => serde_json::to_value(cad_stdlib::lookup_iso10642_csink(query)?),
        StdlibKind::Tap => Ok(json!({
            "tap_drill_mm": cad_stdlib::lookup_metric_tap_drill(query)?,
        })),
        StdlibKind::Bearing => serde_json::to_value(cad_stdlib::lookup_bearing(query)?),
        StdlibKind::Material => serde_json::to_value(cad_stdlib::lookup_material(query)?),
        StdlibKind::Iso286 => {
            let nominal_mm =
                nominal_mm.context("--nominal-mm is required for the iso286 lookup")?;
            serde_json::to_value(cad_stdlib::lookup_iso286(nominal_mm, query)?)
        }
    }?;
    let Value::Object(mut fields) = result else {
        bail!("standard-library result has no named fields");
    };
    let mut text = format!("{kind_name} {query}");
    for (key, value) in &fields {
        use std::fmt::Write as _;
        writeln!(text).expect("writing to a string");
        write!(text, "  {key} = ").expect("writing to a string");
        if let Some(value) = value.as_str() {
            text.push_str(value);
        } else {
            write!(text, "{value}").expect("writing to a string");
        }
    }
    fields.insert("status".to_owned(), json!("ok"));
    fields.insert("kind".to_owned(), json!(kind_name));
    fields.insert("query".to_owned(), json!(query));
    Ok(CommandOutput::report(text, Value::Object(fields)))
}

fn kind_name(kind: &StdlibKind) -> &'static str {
    match kind {
        StdlibKind::Cbore => "cbore",
        StdlibKind::Csink => "csink",
        StdlibKind::Tap => "tap",
        StdlibKind::Bearing => "bearing",
        StdlibKind::Material => "material",
        StdlibKind::Iso286 => "iso286",
    }
}

struct ImplicitFieldSpec {
    field: Box<dyn ScalarField>,
    bbox_min_mm: [f64; 3],
    bbox_max_mm: [f64; 3],
}

fn slice_project(
    path: &Path,
    out_cli: &Path,
    out_svg: Option<&Path>,
    layer_height_mm: f64,
    xy_step_mm: f64,
) -> Result<CommandOutput> {
    if out_svg.is_some_and(|path| path == out_cli) {
        bail!("--out-cli and --out-svg must be different files");
    }
    let project = ProjectFormat::detect(path)?.load(path)?;
    let output_id = project
        .graph
        .outputs
        .iter()
        .find(|id| {
            project
                .graph
                .nodes
                .get(&id.0)
                .is_some_and(|node| node.output_type == IrType::Solid)
        })
        .context("project graph has no declared primary solid output for slicing")?;
    let spec = build_implicit_node(&project.graph, output_id)?;
    let bbox_min_mm = spec.bbox_min_mm;
    let bbox_max_mm = spec.bbox_max_mm;
    // Keep the analytic domain boundary inside a sampling cell so a boundary
    // value of exactly zero cannot be emitted as an open manufacturing path.
    let xy_pad_mm = xy_step_mm * 1.5;
    let config = SliceConfig {
        bbox_min_mm: [
            bbox_min_mm[0] - xy_pad_mm,
            bbox_min_mm[1] - xy_pad_mm,
            bbox_min_mm[2],
        ],
        bbox_max_mm: [
            bbox_max_mm[0] + xy_pad_mm,
            bbox_max_mm[1] + xy_pad_mm,
            bbox_max_mm[2],
        ],
        layer_height_mm,
        xy_step_mm,
        stitch_tol_mm: xy_step_mm * 1.0e-5,
    };
    let source = spec.field;
    let adapter = FnSliceField::new(move |x, y, z| source.eval([x, y, z]));
    let stack = slice_field(&adapter, &config).context("cannot slice implicit field")?;
    let cli = export_cli_ascii(&stack, 1.0).context("cannot export CLI v2.0 contour stack")?;
    let svg = if let Some(out_svg) = out_svg {
        let non_empty: Vec<_> = stack
            .layers
            .iter()
            .filter(|layer| !layer.loops.is_empty())
            .collect();
        let layer = non_empty
            .get(non_empty.len() / 2)
            .context("sliced field contains no non-empty layers for SVG export")?;
        Some((
            out_svg,
            export_layer_svg(
                layer,
                [config.bbox_min_mm[0], config.bbox_min_mm[1]],
                [config.bbox_max_mm[0], config.bbox_max_mm[1]],
            ),
        ))
    } else {
        None
    };
    fs::write(out_cli, cli)
        .with_context(|| format!("cannot write CLI output `{}`", out_cli.display()))?;
    if let Some((out_svg, svg)) = svg {
        fs::write(out_svg, svg)
            .with_context(|| format!("cannot write SVG output `{}`", out_svg.display()))?;
    }

    let total_loops = stack
        .layers
        .iter()
        .map(|layer| layer.loops.len())
        .sum::<usize>();
    Ok(CommandOutput::report(
        format!("Wrote CLI contour stack to {}", out_cli.display()),
        json!({
            "status": "ok",
            "layer_count": stack.layers.len(),
            "total_loops": total_loops,
            "out_cli": out_cli.to_string_lossy(),
            "out_svg": out_svg.map(|path| path.to_string_lossy().into_owned()),
        }),
    ))
}

fn render_drawing_project(
    path: &Path,
    sheet_id: &str,
    out_svg: &Path,
    kernel: &dyn GeometryKernel,
) -> Result<CommandOutput> {
    let project = ProjectFormat::detect(path)?.load(path)?;
    let sheet = project
        .drawings
        .get(sheet_id)
        .with_context(|| format!("drawing sheet `{sheet_id}` was not found in project"))?;
    let mut evaluator = GraphEvaluator::new(kernel);
    let report = evaluator
        .evaluate_project_at_path(path, None)
        .context("cannot evaluate project graph for drawing")?;
    let output_id = project
        .graph
        .outputs
        .iter()
        .find(|id| {
            project
                .graph
                .nodes
                .get(&id.0)
                .is_some_and(|node| node.output_type == IrType::Solid)
        })
        .context("project graph has no declared primary solid output for drawing")?;
    let node_report = report
        .node_reports
        .get(output_id)
        .context("primary drawing output was not evaluated")?;
    if !matches!(&node_report.status, NodeState::Valid) {
        let detail = node_report
            .error
            .as_deref()
            .unwrap_or("node did not evaluate to a valid solid");
        bail!("primary drawing output `{output_id}` failed evaluation: {detail}");
    }
    let solid = report
        .output_solids
        .get(output_id)
        .context("primary drawing output solid is unavailable")?;
    let snapshot = evaluator
        .cache()
        .get(output_id)
        .and_then(|node| node.topo_snapshot.as_ref())
        .context("primary drawing output has no topology snapshot")?;
    let resolved = resolve_drawing_sheet(sheet, snapshot, &MatchTolerances::default());
    let svg = render_associative_sheet_svg(&resolved, solid);
    fs::write(out_svg, svg)
        .with_context(|| format!("cannot write drawing SVG `{}`", out_svg.display()))?;
    let resolved_dimensions = resolved
        .dimensions
        .iter()
        .filter(|dimension| matches!(&dimension.status, DimensionStatus::Resolved))
        .count();
    let dangling_dimensions = resolved.dimensions.len() - resolved_dimensions;
    Ok(CommandOutput::report(
        format!("Wrote drawing sheet {sheet_id} to {}", out_svg.display()),
        json!({
            "status": "ok",
            "sheet_id": sheet_id,
            "resolved_dimensions": resolved_dimensions,
            "dangling_dimensions": dangling_dimensions,
            "out_svg": out_svg.to_string_lossy(),
        }),
    ))
}

fn export_3mf_project(
    path: &Path,
    out: &Path,
    resolution: Option<usize>,
    kernel: &dyn GeometryKernel,
) -> Result<CommandOutput> {
    if resolution == Some(0) {
        bail!("--resolution must be a positive integer");
    }
    let storage = ProjectFormat::detect(path)?;
    let project = storage.load(path)?;
    let mut graph = project.graph;
    if let Some(resolution) = resolution {
        let resolution =
            i64::try_from(resolution).context("--resolution is too large for the project graph")?;
        for node in graph.nodes.values_mut() {
            if matches!(node.kind, NodeKind::ImplicitLatticeInfill) {
                node.inputs
                    .insert("resolution".to_owned(), IrValue::Int(resolution));
            }
        }
    }
    let mut evaluator = GraphEvaluator::new(kernel);
    let report = if resolution.is_none() {
        evaluator
            .evaluate_project_at_path(path, None)
            .context("cannot evaluate project graph for 3MF export")?
    } else {
        evaluator.set_external_ref_roots(Some(path.to_path_buf()), None);
        evaluator
            .evaluate_graph_with_blobs(&graph, &project.blobs, kernel)
            .context("cannot evaluate project graph for 3MF export")?
    };
    let (mesh, track) = primary_export_mesh(&graph, &report)?;
    write_3mf_mesh_file(out, &mesh, &project.manifest.project_name)
        .with_context(|| format!("cannot write 3MF output `{}`", out.display()))?;
    Ok(CommandOutput::report(
        format!(
            "Wrote 3MF mesh to {} ({} vertices, {} triangles)",
            out.display(),
            mesh.vertices.len(),
            mesh.triangles.len()
        ),
        json!({
            "status": "ok",
            "out": out.to_string_lossy(),
            "vertex_count": mesh.vertices.len(),
            "triangle_count": mesh.triangles.len(),
            "track": track,
        }),
    ))
}

fn primary_export_mesh(
    graph: &CanonicalGraph,
    report: &cad_evaluator::GraphEvalReport,
) -> Result<(Mesh, &'static str)> {
    let id = graph
        .outputs
        .iter()
        .find(|id| {
            graph.nodes.get(&id.0).is_some_and(|node| {
                matches!(
                    node.output_type,
                    IrType::Solid | IrType::Surface | IrType::SubD
                )
            })
        })
        .context("project graph has no declared mesh output")?;
    let node_report = report
        .node_reports
        .get(id)
        .with_context(|| format!("primary output `{id}` was not evaluated"))?;
    if !matches!(&node_report.status, NodeState::Valid) {
        let detail = node_report
            .error
            .as_deref()
            .unwrap_or("output node did not evaluate successfully");
        bail!("primary output `{id}` failed evaluation: {detail}");
    }
    if let Some(output) = report.output_implicit_meshes.get(id) {
        let (tri_chunks, rem) = output.mesh.indices.as_chunks::<3>();
        if !rem.is_empty() {
            bail!("implicit output `{id}` has an incomplete triangle index list");
        }
        let mesh = Mesh {
            vertices: output.mesh.vertices.clone(),
            triangles: tri_chunks.to_vec(),
        };
        return Ok((mesh, "tpms"));
    }
    if let Some(output) = report.output_subd_meshes.get(id) {
        return Ok((
            Mesh {
                vertices: output.vertices.clone(),
                triangles: output.triangles.clone(),
            },
            "subd",
        ));
    }
    if let Some(output) = report.output_solids.get(id) {
        return Ok((solid_mesh(output)?, "brep"));
    }
    bail!("project graph did not produce a primary mesh output")
}

fn solid_mesh(solid: &cad_kernel_occt::SolidShape) -> Result<Mesh> {
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let mut slots = HashMap::<(u64, u64, u64), u32>::new();
    for (face_index, face) in solid.faces.iter().enumerate() {
        let (vert_chunks, vert_rem) = face.vertices.as_chunks::<3>();
        if !vert_rem.is_empty() {
            bail!("B-Rep face {face_index} has incomplete vertex coordinates");
        }
        let (idx_chunks, idx_rem) = face.indices.as_chunks::<3>();
        if !idx_rem.is_empty() {
            bail!("B-Rep face {face_index} has incomplete triangle indices");
        }
        let mut local = Vec::with_capacity(vert_chunks.len());
        for &point in vert_chunks {
            let key = (
                normalized_coordinate_bits(point[0]),
                normalized_coordinate_bits(point[1]),
                normalized_coordinate_bits(point[2]),
            );
            let index = if let Some(&index) = slots.get(&key) {
                index
            } else {
                let index = u32::try_from(vertices.len())
                    .context("B-Rep mesh has too many unique vertices")?;
                vertices.push(point);
                slots.insert(key, index);
                index
            };
            local.push(index);
        }
        for index in idx_chunks {
            let remap = |index: u32| {
                local.get(index as usize).copied().ok_or_else(|| {
                    anyhow::anyhow!("B-Rep face {face_index} references an invalid vertex")
                })
            };
            triangles.push([remap(index[0])?, remap(index[1])?, remap(index[2])?]);
        }
    }
    Ok(Mesh {
        vertices,
        triangles,
    })
}

fn normalized_coordinate_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0_f64.to_bits()
    } else {
        value.to_bits()
    }
}

fn build_implicit_node(graph: &CanonicalGraph, id: &NodeId) -> Result<ImplicitFieldSpec> {
    let node = graph
        .nodes
        .get(&id.0)
        .with_context(|| format!("implicit field references missing node `{id}`"))?;
    if matches!(&node.state, NodeState::Suppressed) {
        bail!("implicit field node `{id}` is suppressed");
    }
    match &node.kind {
        NodeKind::PrimitiveBox => {
            let [width, depth, height] = if let Some(value) = node.inputs.get("size") {
                implicit_vec3(graph, value, "size")?
            } else {
                [
                    input_length(graph, node, &["width"])?,
                    input_length(graph, node, &["depth", "length"])?,
                    input_length(graph, node, &["height"])?,
                ]
            };
            let width = positive_implicit(width, "box width")?;
            let depth = positive_implicit(depth, "box depth")?;
            let height = positive_implicit(height, "box height")?;
            let origin = input_vec3(graph, node, "origin")?.unwrap_or([0.0; 3]);
            Ok(ImplicitFieldSpec {
                field: Box::new(BoxSdf::new(
                    [
                        origin[0] + width * 0.5,
                        origin[1] + depth * 0.5,
                        origin[2] + height * 0.5,
                    ],
                    [width * 0.5, depth * 0.5, height * 0.5],
                )),
                bbox_min_mm: origin,
                bbox_max_mm: [origin[0] + width, origin[1] + depth, origin[2] + height],
            })
        }
        NodeKind::PrimitiveCylinder => {
            let diameter = if node.inputs.contains_key("d") || node.inputs.contains_key("diameter")
            {
                input_length(graph, node, &["d", "diameter"])?
            } else {
                2.0 * input_length(graph, node, &["radius"])?
            };
            let diameter = positive_implicit(diameter, "cylinder diameter")?;
            let height = positive_implicit(
                input_length(graph, node, &["h", "height"])?,
                "cylinder height",
            )?;
            let origin = input_vec3(graph, node, "origin")?.unwrap_or([0.0; 3]);
            let radius = diameter * 0.5;
            Ok(ImplicitFieldSpec {
                field: Box::new(CylinderSdf::new(
                    [origin[0], origin[1], origin[2] + height * 0.5],
                    radius,
                    height * 0.5,
                )),
                bbox_min_mm: [origin[0] - radius, origin[1] - radius, origin[2]],
                bbox_max_mm: [origin[0] + radius, origin[1] + radius, origin[2] + height],
            })
        }
        NodeKind::FieldTpms { .. } => bail!(
            "FieldTpms is a scalar field and must be consumed by an ImplicitLatticeInfill output"
        ),
        NodeKind::ImplicitLatticeInfill => {
            let domain_ref = input_node_ref(node, &["solid", "base", "target", "body"])?;
            let domain = build_implicit_node(graph, &domain_ref)?;
            let field_ref = input_node_ref(node, &["field", "lattice"])?;
            let field_node = graph
                .nodes
                .get(&field_ref.0)
                .with_context(|| format!("lattice field references missing node `{field_ref}`"))?;
            let NodeKind::FieldTpms { family } = &field_node.kind else {
                bail!("implicit lattice field must reference a FieldTpms node")
            };
            if matches!(&field_node.state, NodeState::Suppressed) {
                bail!("implicit lattice field node `{field_ref}` is suppressed");
            }
            let tpms = build_tpms(graph, field_node, *family)?;
            let wall_thickness = optional_input_length(graph, node, &["wall_thickness"])?;
            let explicit_skin = optional_input_length(graph, node, &["skin_thickness_mm"])?;
            let has_field_thickness = tpms.thickness_mm.is_some();
            let sheet_thickness = tpms.thickness_mm.or(wall_thickness).unwrap_or(0.5);
            let skin_thickness =
                explicit_skin.or_else(|| has_field_thickness.then_some(wall_thickness).flatten());
            let sheet_thickness = positive_implicit(sheet_thickness, "TPMS sheet thickness")?;
            let skin_thickness = skin_thickness
                .map(|value| positive_implicit(value, "lattice skin thickness"))
                .transpose()?;
            let bbox_min_mm = domain.bbox_min_mm;
            let bbox_max_mm = domain.bbox_max_mm;
            let field = LatticeInfillSdf::new(
                domain.field,
                TpmsSheetField::new(tpms.field, Box::new(ConstantField(sheet_thickness * 0.5))),
            );
            let field = if let Some(skin) = skin_thickness {
                field.with_skin(skin)
            } else {
                field
            };
            Ok(ImplicitFieldSpec {
                field: Box::new(field),
                bbox_min_mm,
                bbox_max_mm,
            })
        }
        other => bail!("unsupported implicit slicing node `{id}` of kind {other:?}"),
    }
}

fn build_tpms(
    graph: &CanonicalGraph,
    node: &cad_ir::IrNode,
    family: cad_ir::TpmsFamily,
) -> Result<TpmsSpec> {
    if matches!(&node.state, NodeState::Suppressed) {
        bail!("TPMS field node `{}` is suppressed", node.id);
    }
    let period = match node
        .inputs
        .get("period")
        .or_else(|| node.inputs.get("period_mm"))
        .or_else(|| node.inputs.get("cell_size"))
    {
        Some(value) => match resolve_implicit_value(graph, value)? {
            IrValue::Vec3(period) => *period,
            other => {
                let value = implicit_number(other, "TPMS period")?;
                [value; 3]
            }
        },
        None => [10.0; 3],
    };
    if period
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        bail!("TPMS period must be finite and positive")
    }
    if period
        .iter()
        .any(|value| !(std::f64::consts::TAU / *value).is_finite())
    {
        bail!("TPMS period is too small for finite wave numbers")
    }
    let thickness_mm = node
        .inputs
        .get("thickness_mm")
        .or_else(|| node.inputs.get("thickness"))
        .map(|value| {
            let value = implicit_number(resolve_implicit_value(graph, value)?, "TPMS thickness")?;
            positive_implicit(value, "TPMS thickness")
        })
        .transpose()?;
    Ok(TpmsSpec {
        field: TpmsField::new(family, period),
        thickness_mm,
    })
}

struct TpmsSpec {
    field: TpmsField,
    thickness_mm: Option<f64>,
}

fn input_node_ref(node: &cad_ir::IrNode, aliases: &[&str]) -> Result<NodeId> {
    for alias in aliases {
        if let Some(IrValue::NodeRef { node_id, .. }) = node.inputs.get(*alias) {
            return Ok(node_id.clone());
        }
    }
    bail!("node `{}` is missing one of inputs {:?}", node.id, aliases)
}

fn input_length(graph: &CanonicalGraph, node: &cad_ir::IrNode, aliases: &[&str]) -> Result<f64> {
    for alias in aliases {
        if let Some(value) = node.inputs.get(*alias) {
            return implicit_number(resolve_implicit_value(graph, value)?, alias);
        }
    }
    bail!("node `{}` is missing input `{}`", node.id, aliases[0])
}

fn optional_input_length(
    graph: &CanonicalGraph,
    node: &cad_ir::IrNode,
    aliases: &[&str],
) -> Result<Option<f64>> {
    for alias in aliases {
        if node.inputs.contains_key(*alias) {
            return Ok(Some(input_length(graph, node, aliases)?));
        }
    }
    Ok(None)
}

pub(super) fn input_vec3(
    graph: &CanonicalGraph,
    node: &cad_ir::IrNode,
    name: &str,
) -> Result<Option<[f64; 3]>> {
    let Some(value) = node.inputs.get(name) else {
        return Ok(None);
    };
    Ok(Some(implicit_vec3(graph, value, name)?))
}

fn implicit_vec3(graph: &CanonicalGraph, value: &IrValue, label: &str) -> Result<[f64; 3]> {
    match resolve_implicit_value(graph, value)? {
        IrValue::Vec3(value) => Ok(*value),
        IrValue::List(values) if values.len() == 3 => Ok([
            implicit_number(resolve_implicit_value(graph, &values[0])?, label)?,
            implicit_number(resolve_implicit_value(graph, &values[1])?, label)?,
            implicit_number(resolve_implicit_value(graph, &values[2])?, label)?,
        ]),
        _ => bail!("{label} must be a Vec3 or a three-item length list"),
    }
}

fn resolve_implicit_value<'a>(
    graph: &'a CanonicalGraph,
    value: &'a IrValue,
) -> Result<&'a IrValue> {
    let IrValue::NodeRef { node_id, .. } = value else {
        return Ok(value);
    };
    let node = graph
        .nodes
        .get(&node_id.0)
        .with_context(|| format!("value references missing node `{node_id}`"))?;
    if matches!(&node.state, NodeState::Suppressed) {
        bail!("parameter node `{node_id}` is suppressed");
    }
    let NodeKind::ParameterRef { param_id } = &node.kind else {
        bail!("node reference `{node_id}` is not a parameter value")
    };
    let parameter = graph
        .parameters
        .get(&param_id.0)
        .with_context(|| format!("parameter `{param_id}` is missing"))?;
    resolve_implicit_value(graph, &parameter.value)
}

fn implicit_number(value: &IrValue, label: &str) -> Result<f64> {
    let number = match value {
        IrValue::Quantity(quantity) if quantity.ir_type() == IrType::Length => {
            quantity.canonical_value()
        }
        IrValue::Quantity(_) => bail!("{label} must be a length quantity"),
        IrValue::Scalar(value) => *value,
        IrValue::Int(value) => *value as f64,
        _ => bail!("{label} must be a scalar length"),
    };
    if number.is_finite() {
        Ok(number)
    } else {
        bail!("{label} must be finite")
    }
}

fn positive_implicit(value: f64, label: &str) -> Result<f64> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        bail!("{label} must be finite and positive")
    }
}

//! Container manifest and versioned schema metadata.

use cad_ir::{CanonicalGraph, IR_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

use crate::ContainerError;

/// The on-disk `.aicad` container schema supported by this crate.
pub const CONTAINER_FORMAT_VERSION: u32 = 1;
/// The conventional path of the primary KCL projection.
pub const DEFAULT_KCL_VIEW_PATH: &str = "views/main.kcl";
/// The conventional header path for sharded graph directories.
pub const SHARDED_ENTRY_GRAPH_PATH: &str = "graphs/00_header.ir.json";

/// Controls whether a `.kcl` text projection is materialized on disk or synthesized dynamically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KclProjectionPolicy {
    /// Persist only the canonical graph IR; project `.kcl` dynamically on demand to avoid dual-source drift.
    OnDemandDynamic,
    /// Materialize `views/main.kcl` alongside the canonical graph when `kcl_view` is present.
    #[default]
    MaterializedSynced,
}

impl KclProjectionPolicy {
    /// Returns true when the policy matches the backwards-compatible default (`MaterializedSynced`).
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::MaterializedSynced)
    }

    /// Canonical string representation used in relational storage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnDemandDynamic => "on_demand_dynamic",
            Self::MaterializedSynced => "materialized_synced",
        }
    }

    /// Parse a policy name stored in relational metadata.
    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "on_demand_dynamic" => Some(Self::OnDemandDynamic),
            "materialized_synced" => Some(Self::MaterializedSynced),
            _ => None,
        }
    }
}

/// The kernel implementation that produced a project or cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelFingerprint {
    pub kernel_id: String,
    pub kernel_version: String,
}

impl KernelFingerprint {
    pub fn new(kernel_id: impl Into<String>, kernel_version: impl Into<String>) -> Self {
        Self {
            kernel_id: kernel_id.into(),
            kernel_version: kernel_version.into(),
        }
    }
}

/// The top-level metadata needed to interpret a project container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerManifest {
    pub format_version: u32,
    pub ir_schema_version: u32,
    pub project_name: String,
    pub created_with_kernel: KernelFingerprint,
    pub entry_graph: String,
    #[serde(default)]
    pub kcl_view: Option<String>,
    #[serde(default, skip_serializing_if = "KclProjectionPolicy::is_default")]
    pub kcl_projection_policy: KclProjectionPolicy,
}

impl ContainerManifest {
    /// Creates a manifest for the current container and graph schemas.
    pub fn new(project_name: impl Into<String>, kernel: KernelFingerprint) -> Self {
        Self {
            format_version: CONTAINER_FORMAT_VERSION,
            ir_schema_version: IR_SCHEMA_VERSION,
            project_name: project_name.into(),
            created_with_kernel: kernel,
            entry_graph: "graph.ir.json".to_owned(),
            kcl_view: Some(DEFAULT_KCL_VIEW_PATH.to_owned()),
            kcl_projection_policy: KclProjectionPolicy::default(),
        }
    }
}

#[derive(Deserialize)]
struct ManifestVersions {
    format_version: u64,
    ir_schema_version: Option<u64>,
}

#[derive(Deserialize)]
struct GraphVersion {
    #[serde(default)]
    ir_schema_version: Option<u64>,
}

/// Check versions before strictly decoding the manifest or following its paths.
pub(crate) fn parse_manifest(manifest_json: &str) -> Result<ContainerManifest, ContainerError> {
    let versions: ManifestVersions = serde_json::from_str(manifest_json)?;
    validate_version(
        "container format",
        versions.format_version,
        CONTAINER_FORMAT_VERSION,
    )?;
    if let Some(found) = versions.ir_schema_version {
        validate_version("manifest IR schema", found, IR_SCHEMA_VERSION)?;
    }
    Ok(serde_json::from_str(manifest_json)?)
}

/// Preflight the graph version before decoding and validating the graph itself.
pub(crate) fn parse_graph(graph_json: &str) -> Result<CanonicalGraph, ContainerError> {
    let version: GraphVersion = serde_json::from_str(graph_json)?;
    if let Some(found) = version.ir_schema_version {
        validate_graph_schema_version(found)?;
    }
    Ok(CanonicalGraph::from_json(graph_json)?)
}

pub(crate) fn validate_graph_schema_version(found: u64) -> Result<(), ContainerError> {
    validate_version("graph IR schema", found, IR_SCHEMA_VERSION)
}

pub(crate) fn validate_versions(
    manifest: &ContainerManifest,
    graph_schema_version: u32,
) -> Result<(), ContainerError> {
    validate_manifest_versions(
        manifest.format_version.into(),
        manifest.ir_schema_version.into(),
    )?;
    validate_graph_schema_version(graph_schema_version.into())
}

pub(crate) fn validate_manifest_versions(format: u64, ir: u64) -> Result<(), ContainerError> {
    validate_version("container format", format, CONTAINER_FORMAT_VERSION)?;
    validate_version("manifest IR schema", ir, IR_SCHEMA_VERSION)
}

fn validate_version(
    component: &'static str,
    found: u64,
    supported: u32,
) -> Result<(), ContainerError> {
    if found == 0 || found > u64::from(supported) {
        return Err(ContainerError::UnsupportedVersion {
            component,
            found,
            supported,
        });
    }
    Ok(())
}

//! Optional JSONL file sink (`sink` feature). Only process entry points use it.

mod layer;
mod rotate;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;

pub use layer::{JsonlLayer, format_rfc3339};
pub use rotate::{DEFAULT_KEEP, DEFAULT_MAX_BYTES, RotatingFile};

#[derive(Debug, Clone)]
pub struct SinkConfig {
    /// Process name; also the file stem, e.g. `cad-cli` -> `cad-cli.jsonl`.
    pub process_name: String,
    /// Filter used when `AICAD_LOG` is unset or invalid.
    pub default_level: String,
    /// Overrides [`default_log_dir`].
    pub log_dir: Option<PathBuf>,
    /// Used to turn `path` fields workspace-relative.
    pub workspace_root: Option<PathBuf>,
    pub max_bytes: u64,
    pub keep: usize,
}

impl SinkConfig {
    pub fn new(process_name: impl Into<String>, default_level: impl Into<String>) -> Self {
        Self {
            process_name: process_name.into(),
            default_level: default_level.into(),
            log_dir: None,
            workspace_root: None,
            max_bytes: DEFAULT_MAX_BYTES,
            keep: DEFAULT_KEEP,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SinkHandle {
    pub trace_id: String,
    pub log_path: PathBuf,
}

#[derive(Debug)]
pub enum SinkError {
    Io(std::io::Error),
    AlreadyInitialized,
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot open runtime log: {e}"),
            Self::AlreadyInitialized => f.write_str("a global tracing subscriber already exists"),
        }
    }
}

impl std::error::Error for SinkError {}

pub use crate::log_dir::default_log_dir;

/// Path of the JSONL file for `process_name` inside `dir`.
pub fn log_file_path(dir: &Path, process_name: &str) -> PathBuf {
    dir.join(format!("{process_name}.jsonl"))
}

fn build_filter(default_level: &str) -> EnvFilter {
    crate::env::log_directive()
        .and_then(|d| EnvFilter::try_new(d).ok())
        .or_else(|| EnvFilter::try_new(default_level).ok())
        .unwrap_or_else(|| EnvFilter::new("info"))
}

/// Install the global subscriber writing JSONL to `<logs>/<process>.jsonl`.
/// Failures are returned, never panic; callers should ignore them.
pub fn init(config: SinkConfig) -> Result<SinkHandle, SinkError> {
    let dir = config.log_dir.clone().unwrap_or_else(default_log_dir);
    let log_path = log_file_path(&dir, &config.process_name);
    let file = RotatingFile::open(log_path.clone(), config.max_bytes, config.keep)
        .map_err(SinkError::Io)?;
    let trace_id = crate::ids::init_process_trace_id_from_env().to_owned();
    let layer = JsonlLayer::new(
        Arc::new(Mutex::new(file)),
        config.process_name,
        trace_id.clone(),
        config.workspace_root,
    );
    let subscriber = tracing_subscriber::registry()
        .with(build_filter(&config.default_level))
        .with(layer);
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| SinkError::AlreadyInitialized)?;
    Ok(SinkHandle { trace_id, log_path })
}

/// Build the layer pieces for tests or custom composition.
pub fn layer_for_file(
    file: RotatingFile,
    process_name: &str,
    trace_id: &str,
    workspace_root: Option<PathBuf>,
) -> JsonlLayer {
    JsonlLayer::new(
        Arc::new(Mutex::new(file)),
        process_name.to_owned(),
        trace_id.to_owned(),
        workspace_root,
    )
}

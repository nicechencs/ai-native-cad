//! Runtime observability contract shared by all CAD processes.
//!
//! Business crates only depend on `tracing` through this crate. Writing events
//! to disk is the job of the optional `sink` feature, enabled by process entry
//! points only. See `schemas/runtime-event.schema.json`.

pub mod codes;
pub mod env;
pub mod ids;
pub mod log_dir;
pub mod sanitize;
#[cfg(feature = "sink")]
pub mod sink;

pub use ids::{
    current_trace_id, new_span_id, new_trace_id, resolve_trace_id, set_process_trace_id,
};
pub use sanitize::{sanitize_message, sanitize_path, truncate_message};
pub use tracing;

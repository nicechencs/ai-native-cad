//! `trace_id` / `span_id` generation and process-wide trace id.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env;

static COUNTER: AtomicU64 = AtomicU64::new(0);
static PROCESS_TRACE_ID: OnceLock<String> = OnceLock::new();

fn random_u64() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(nanos);
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    hasher.finish()
}

/// 32 lowercase hex characters.
pub fn new_trace_id() -> String {
    format!("{:016x}{:016x}", random_u64(), random_u64())
}

/// 16 lowercase hex characters.
pub fn new_span_id() -> String {
    format!("{:016x}", random_u64())
}

/// Accepts ids that are safe to propagate: 8..=64 chars of `[0-9A-Za-z_-]`.
pub fn is_valid_id(value: &str) -> bool {
    (8..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Use the supplied id when valid, otherwise generate a fresh one.
pub fn resolve_trace_id(candidate: Option<&str>) -> String {
    match candidate.map(str::trim) {
        Some(value) if is_valid_id(value) => value.to_owned(),
        _ => new_trace_id(),
    }
}

/// Fix the process trace id (first call wins) and return the effective value.
pub fn set_process_trace_id(candidate: Option<&str>) -> &'static str {
    PROCESS_TRACE_ID.get_or_init(|| resolve_trace_id(candidate))
}

/// The process trace id, if one was set.
pub fn current_trace_id() -> Option<&'static str> {
    PROCESS_TRACE_ID.get().map(String::as_str)
}

/// Initialise the process trace id from `AICAD_TRACE_ID` (generated when absent).
pub fn init_process_trace_id_from_env() -> &'static str {
    set_process_trace_id(env::trace_id().as_deref())
}

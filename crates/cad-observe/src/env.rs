//! Environment variable contract.

use crate::ids::is_valid_id;

pub const TRACE_ID: &str = "AICAD_TRACE_ID";
pub const PARENT_SPAN_ID: &str = "AICAD_PARENT_SPAN_ID";
pub const LOG: &str = "AICAD_LOG";
pub const LOG_DIR: &str = "AICAD_LOG_DIR";

fn valid_id_var(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let value = value.trim();
    is_valid_id(value).then(|| value.to_owned())
}

/// `AICAD_TRACE_ID` when set to a valid id.
pub fn trace_id() -> Option<String> {
    valid_id_var(TRACE_ID)
}

/// `AICAD_PARENT_SPAN_ID` when set to a valid id.
pub fn parent_span_id() -> Option<String> {
    valid_id_var(PARENT_SPAN_ID)
}

/// Raw `AICAD_LOG` filter directive (e.g. `debug`, `cad_cli=trace,info`, `off`).
pub fn log_directive() -> Option<String> {
    let value = std::env::var(LOG).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

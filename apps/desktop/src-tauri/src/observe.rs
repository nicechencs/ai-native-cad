//! Desktop host runtime logging: process sink init and per-command root spans.

use std::time::Instant;

use cad_observe::{codes, tracing};

/// Install the `desktop.jsonl` sink (default `info`, `AICAD_LOG` overrides).
/// Failures never affect the application.
pub fn init() {
    let config = cad_observe::sink::SinkConfig::new("desktop", "info");
    let _ = cad_observe::sink::init(config);
}

fn error_code(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("write") || lower.contains("create") || lower.contains("directory") {
        codes::R016_IO_ERROR
    } else {
        codes::R011_IPC_FAILED
    }
}

/// Run an IPC command inside a root span carrying `trace_id` (generated when
/// absent or invalid), logging the action boundary and any failure.
pub fn traced<T>(
    operation: &'static str,
    trace_id: Option<&str>,
    run: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    let trace_id = cad_observe::resolve_trace_id(trace_id);
    let span = tracing::info_span!(
        "ipc",
        trace_id = %trace_id,
        component = "desktop",
        operation = operation
    );
    let _entered = span.enter();
    tracing::info!("command start");
    let started = Instant::now();
    let result = run(&trace_id);
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    match &result {
        Ok(_) => tracing::info!(duration_ms, "command ok"),
        // A cancelled STEP, DXF, or drawing-SVG publication is not an IPC
        // failure. The web export path records the same CLIENT_ABORTED diagnostic.
        Err(message) if message.trim() == super::EXPORT_CLIENT_ABORTED => {
            tracing::info!(
                duration_ms,
                diagnostic_code = super::EXPORT_CLIENT_ABORTED,
                "command aborted"
            );
        }
        Err(message) => tracing::error!(
            duration_ms,
            code = error_code(message),
            error_type = "CommandError",
            "command failed: {message}"
        ),
    }
    result
}

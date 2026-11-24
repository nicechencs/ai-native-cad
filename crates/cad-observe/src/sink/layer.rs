//! JSONL formatting layer implementing `schemas/runtime-event.schema.json`.

use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use super::rotate::RotatingFile;
use crate::sanitize::{sanitize_message, sanitize_path};

/// Format seconds since the epoch as RFC3339 UTC with millisecond precision.
pub fn format_rfc3339(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now_rfc3339() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_rfc3339(d.as_secs(), d.subsec_millis())
}

fn level_str(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// Fields accepted from spans and events. Anything else is dropped on purpose
/// so prompts, sources, patches and mesh data cannot leak by accident.
const STRING_FIELDS: [&str; 10] = [
    "component",
    "operation",
    "code",
    "error_type",
    "node_id",
    "session_id",
    "native_status",
    "path",
    "trace_id",
    "parent_span_id",
];

#[derive(Default)]
struct Fields {
    message: Option<String>,
    strings: Map<String, Value>,
    duration_ms: Option<Value>,
}

impl Fields {
    fn put_str(&mut self, name: &str, value: String) {
        if name == "message" {
            self.message = Some(value);
        } else if STRING_FIELDS.contains(&name) {
            self.strings.insert(name.to_owned(), Value::String(value));
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put_str(field.name(), value.to_owned());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        let name = field.name();
        if name == "message" || STRING_FIELDS.contains(&name) {
            self.put_str(name, format!("{value:?}"));
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "duration_ms" => self.duration_ms = Some(Value::from(value)),
            "native_status" => self.put_str("native_status", value.to_string()),
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        match field.name() {
            "duration_ms" if value >= 0 => self.duration_ms = Some(Value::from(value)),
            "native_status" => self.put_str("native_status", value.to_string()),
            _ => {}
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "duration_ms"
            && value.is_finite()
            && value >= 0.0
            && let Some(n) = serde_json::Number::from_f64((value * 1000.0).round() / 1000.0)
        {
            self.duration_ms = Some(Value::Number(n));
        }
    }
}

struct SpanData {
    span_id: String,
    parent_span_id: Option<String>,
    trace_id: Option<String>,
    component: Option<String>,
    operation: Option<String>,
    name: &'static str,
}

pub struct JsonlLayer {
    writer: Arc<Mutex<RotatingFile>>,
    process_name: String,
    process_trace_id: String,
    root_span_id: String,
    env_parent_span_id: Option<String>,
    workspace_root: Option<PathBuf>,
}

impl JsonlLayer {
    pub fn new(
        writer: Arc<Mutex<RotatingFile>>,
        process_name: String,
        process_trace_id: String,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            writer,
            process_name: if process_name.is_empty() {
                "unknown".to_owned()
            } else {
                process_name
            },
            process_trace_id: crate::ids::resolve_trace_id(Some(&process_trace_id)),
            root_span_id: crate::new_span_id(),
            env_parent_span_id: crate::env::parent_span_id()
                .filter(|id| crate::ids::is_valid_id(id)),
            workspace_root,
        }
    }
}

impl<S> Layer<S> for JsonlLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let parent = span.parent();
        let parent_data = parent.as_ref().and_then(|p| {
            p.extensions()
                .get::<SpanData>()
                .map(|d| (d.span_id.clone(), d.trace_id.clone()))
        });
        let take =
            |f: &Fields, k: &str| f.strings.get(k).and_then(Value::as_str).map(str::to_owned);
        let explicit_parent =
            take(&fields, "parent_span_id").filter(|value| crate::ids::is_valid_id(value));
        let explicit_trace =
            take(&fields, "trace_id").filter(|value| crate::ids::is_valid_id(value));
        let data = SpanData {
            span_id: crate::new_span_id(),
            parent_span_id: explicit_parent
                .or(parent_data.as_ref().map(|p| p.0.clone()))
                .or_else(|| self.env_parent_span_id.clone()),
            trace_id: explicit_trace.or(parent_data.and_then(|p| p.1)),
            component: take(&fields, "component").filter(|value| !value.is_empty()),
            operation: take(&fields, "operation").filter(|value| !value.is_empty()),
            name: span.name(),
        };
        span.extensions_mut().insert(data);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);

        let mut trace_id = None;
        let mut component = None;
        let mut operation = None;
        let mut span_name = None;
        let mut span_id = None;
        let mut parent_span_id = None;
        if let Some(scope) = ctx.event_scope(event) {
            for (depth, span) in scope.enumerate() {
                let ext = span.extensions();
                let Some(data) = ext.get::<SpanData>() else {
                    continue;
                };
                if depth == 0 {
                    span_id = Some(data.span_id.clone());
                    parent_span_id = data.parent_span_id.clone();
                    span_name = Some(data.name.to_owned());
                }
                trace_id = trace_id.or_else(|| data.trace_id.clone());
                component = component.or_else(|| data.component.clone());
                operation = operation.or_else(|| data.operation.clone());
            }
        }

        let mut take = |k: &str| {
            fields
                .strings
                .remove(k)
                .and_then(|v| v.as_str().map(str::to_owned))
        };
        let trace_id = take("trace_id")
            .filter(|value| crate::ids::is_valid_id(value))
            .or_else(|| trace_id.filter(|value| crate::ids::is_valid_id(value)))
            .unwrap_or_else(|| self.process_trace_id.clone());
        let component = take("component")
            .filter(|value| !value.is_empty())
            .or(component)
            .unwrap_or_else(|| self.process_name.clone());
        let operation = take("operation")
            .filter(|value| !value.is_empty())
            .or(operation)
            .or_else(|| span_name.filter(|value| !value.is_empty()))
            .unwrap_or_else(|| event.metadata().target().to_owned());
        let parent_override =
            take("parent_span_id").filter(|value| crate::ids::is_valid_id(value));
        let span_id = span_id.unwrap_or_else(|| self.root_span_id.clone());
        let parent_span_id = parent_override
            .or(parent_span_id)
            .or_else(|| {
                (span_id == self.root_span_id)
                    .then(|| self.env_parent_span_id.clone())
                    .flatten()
            })
            .filter(|value| crate::ids::is_valid_id(value));

        let mut out = Map::new();
        out.insert("ts".into(), now_rfc3339().into());
        out.insert("level".into(), level_str(event.metadata().level()).into());
        out.insert("trace_id".into(), trace_id.into());
        out.insert("span_id".into(), span_id.into());
        out.insert("component".into(), component.into());
        out.insert("operation".into(), operation.into());
        out.insert(
            "message".into(),
            sanitize_message(fields.message.as_deref().unwrap_or("")).into(),
        );
        if let Some(parent) = parent_span_id {
            out.insert("parent_span_id".into(), parent.into());
        }
        if let Some(duration) = fields.duration_ms.take() {
            out.insert("duration_ms".into(), duration);
        }
        let mut error_type = take("error_type")
            .map(|value| sanitize_message(&value))
            .filter(|value| !value.is_empty());
        if let Some(raw_code) = take("code") {
            let code = sanitize_message(&raw_code);
            if crate::codes::ALL.contains(&code.as_str()) {
                out.insert("code".into(), code.into());
            } else if !code.is_empty() {
                error_type = Some(match error_type {
                    Some(existing) if existing != code => {
                        sanitize_message(&format!("{existing}:{code}"))
                    }
                    _ => code,
                });
            }
        }
        if let Some(err_type) = error_type {
            out.insert("error_type".into(), err_type.into());
        }
        for key in ["node_id", "session_id", "native_status"] {
            if let Some(value) = take(key) {
                out.insert(key.into(), sanitize_message(&value).into());
            }
        }
        if let Some(path) = take("path") {
            out.insert(
                "path".into(),
                sanitize_path(&path, self.workspace_root.as_deref()).into(),
            );
        }

        if let Ok(line) = serde_json::to_string(&Value::Object(out))
            && let Ok(mut writer) = self.writer.lock()
        {
            writer.write_line(&line);
        }
    }
}

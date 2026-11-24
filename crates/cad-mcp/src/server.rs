//! JSON-RPC dispatch for the eight tools and the `aicad://` resources.
//!
//! This module owns the open document session and forwards every tool call.
//! It does not evaluate geometry or edit the graph itself.

use std::path::Path;

use cad_runtime::{DocumentRuntime, DocumentSession};
use serde_json::{Value, json};

use crate::error::GatewayError;
use crate::protocol::{self, failure, parse_request, success};
use crate::resources::{list_resources, read_resource};
use crate::tools::{call_tool, tool_error_result, tools_list_result};

const KNOWN_PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// One MCP session bound to one `.aicad` document.
pub struct McpServer {
    session: DocumentSession,
}

impl McpServer {
    /// Open a headless document. `document` must stay inside `workspace`.
    pub fn open_headless(
        workspace: impl AsRef<Path>,
        document: impl AsRef<Path>,
        project_name: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        let runtime = DocumentRuntime::headless(workspace)?;
        let session = runtime.create_session(document, project_name)?;
        Ok(Self { session })
    }

    pub fn from_session(session: DocumentSession) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &DocumentSession {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut DocumentSession {
        &mut self.session
    }

    /// Handle one JSON-RPC message. Notifications return `None`.
    pub fn handle_message(&mut self, line: &str) -> Option<String> {
        let request = match parse_request(line) {
            Ok(request) => request,
            Err(err) => {
                let code = if err.code == "E_JSONRPC_PARSE" {
                    -32700
                } else {
                    -32600
                };
                return Some(failure(None, code, err.message).to_string());
            }
        };
        let Some(id) = request.id else {
            return None;
        };
        Some(self.dispatch(&id, &request.method, &request.params).to_string())
    }

    fn dispatch(&mut self, id: &Value, method: &str, params: &Value) -> Value {
        match method {
            "initialize" => success(id, initialize_result(params)),
            "ping" => success(id, json!({})),
            "tools/list" => success(id, tools_list_result()),
            "tools/call" => self.tools_call(id, params),
            "resources/list" => success(id, list_resources()),
            "resources/read" => self.resources_read(id, params),
            _ => failure(Some(id), -32601, format!("method not found: {method}")),
        }
    }

    fn tools_call(&mut self, id: &Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return failure(Some(id), -32602, "tools/call requires name");
        };
        let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        match call_tool(&mut self.session, name, &arguments) {
            Ok(result) => success(id, result),
            Err(err) => success(id, tool_error_result(&err)),
        }
    }

    fn resources_read(&mut self, id: &Value, params: &Value) -> Value {
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return failure(Some(id), -32602, "resources/read requires uri");
        };
        match read_resource(uri) {
            Ok(body) => success(id, body),
            Err(err) => failure(Some(id), -32602, err.to_string()),
        }
    }
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let protocol_version = if KNOWN_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        protocol::PROTOCOL_VERSION
    };
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "listChanged": false }
        },
        "serverInfo": {
            "name": protocol::SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

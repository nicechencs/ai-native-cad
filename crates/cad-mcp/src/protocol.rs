//! JSON-RPC 2.0 envelopes for the MCP methods this gateway serves.

use serde_json::{Value, json};

use crate::error::GatewayError;

pub const JSONRPC: &str = "2.0";
pub const SERVER_NAME: &str = "aicad-mcp";
pub const PROTOCOL_VERSION: &str = "2025-03-26";

#[derive(Debug)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

pub fn parse_request(line: &str) -> Result<RpcRequest, GatewayError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|err| GatewayError::new("E_JSONRPC_PARSE", err.to_string()))?;
    if value.is_array() {
        return Err(GatewayError::invalid(
            "JSON-RPC batches are not accepted on this gateway",
        ));
    }
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if method.is_empty() {
        return Err(GatewayError::invalid("JSON-RPC method is missing"));
    }
    if value.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC) {
        return Err(GatewayError::invalid("jsonrpc must be \"2.0\""));
    }
    Ok(RpcRequest {
        id: value.get("id").cloned(),
        method,
        params: value.get("params").cloned().unwrap_or(Value::Null),
    })
}

pub fn success(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": JSONRPC,
        "id": id,
        "result": result,
    })
}

pub fn failure(id: Option<&Value>, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": JSONRPC,
        "id": id.cloned().unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message.into(),
        }
    })
}

pub fn tool_result(payload: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Rough upper bound for cl100k / o200k on compact JSON.
///
/// Counting one token per three UTF-8 bytes overshoots those encodings for
/// this schema, so a value under 3,500 here is under the V-01 budget.
pub fn estimate_tokens(json: &str) -> usize {
    json.len().div_ceil(3)
}

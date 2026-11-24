//! Thin MCP protocol gateway for external agents.
//!
//! This crate encodes JSON-RPC, checks HTTP auth, and routes tool calls.
//! Geometry, graph edits, catalogs, and topology queries stay in
//! `cad-runtime`, `cad-api`, `cad-topo`, and `cad-stdlib`.

#![forbid(unsafe_code)]

mod error;
mod http;
mod protocol;
mod resources;
mod sanitize;
mod server;
mod stdio;
mod tools;
mod verify;

pub use error::GatewayError;
pub use http::{
    HttpAuthError, HttpSecurityConfig, bind_streamable_http, handle_http_request,
    serve_one_http_connection, validate_http_request,
};
pub use sanitize::sanitize_untrusted_label;
pub use server::McpServer;
pub use stdio::serve_stdio;
pub use tools::{
    estimate_token_budget, export_openai_tool_schemas, token_budget_of_tools_list, tool_definitions,
};

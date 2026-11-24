//! Open a headless `.aicad` session (or attach to a live desktop host) and call MCP tools.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cad_mcp::McpServer;
use cad_runtime::{
    DocumentRuntime, DocumentSession, RuntimeDiscoveryInfo, read_runtime_discovery,
    resolve_in_workspace, sha256_hex,
};
use serde_json::{Value, json};

const LIVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const LIVE_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct HostError {
    pub code: String,
    pub message: String,
}

impl HostError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn from_runtime(err: cad_runtime::RuntimeError) -> Self {
        Self {
            code: err.code().to_owned(),
            message: err.to_string(),
        }
    }
}

/// Authenticated single-request client for a live desktop loopback MCP host.
pub(crate) struct LiveClient {
    port: u16,
    path: String,
    token: String,
}

/// Probe the workspace for an active desktop Live-Attach host unless `headless` is set.
pub(crate) fn try_live_client(workspace: &Path, headless: bool) -> Option<LiveClient> {
    if headless {
        return None;
    }
    let _target = crate::live_proxy::try_attach(workspace)?;
    let info = read_runtime_discovery(workspace).ok()?;
    let (port, path) = parse_loopback_endpoint(&info.ipc_endpoint)?;
    let token = resolve_bearer_token(&info)?;
    Some(LiveClient { port, path, token })
}

impl LiveClient {
    pub(crate) fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
            }
        });
        let raw = post_json(self, &request.to_string()).map_err(|err| {
            HostError::new("E_IO", format!("live-attach MCP tool `{name}` failed: {err}"))
        })?;
        parse_mcp_tool_response(name, &raw)
    }
}

pub(crate) fn require_project(project: Option<&Path>) -> Result<&Path, HostError> {
    project.ok_or_else(|| {
        HostError::new(
            "E400_INVALID_ARGUMENT",
            "--project is required when no desktop host is active",
        )
    })
}

pub(crate) fn open_existing(
    workspace: &Path,
    project: &Path,
) -> Result<DocumentSession, HostError> {
    let runtime = DocumentRuntime::headless(workspace).map_err(HostError::from_runtime)?;
    let resolved =
        resolve_in_workspace(runtime.workspace_root(), project).map_err(HostError::from_runtime)?;
    if !resolved.is_file() {
        return Err(HostError::new(
            "E_IO",
            format!("project `{}` does not exist", project.display()),
        ));
    }
    tracing::debug!(
        component = "cad-cli",
        operation = "runtime_open_existing",
        eval_path = "graph_evaluator",
        project = %resolved.display(),
        "opening existing headless .aicad session"
    );
    runtime
        .open_session(&resolved)
        .map_err(HostError::from_runtime)
}

pub(crate) fn open_or_create(
    workspace: &Path,
    project: &Path,
) -> Result<DocumentSession, HostError> {
    let runtime = DocumentRuntime::headless(workspace).map_err(HostError::from_runtime)?;
    let resolved =
        resolve_in_workspace(runtime.workspace_root(), project).map_err(HostError::from_runtime)?;
    if resolved.is_file() {
        tracing::debug!(
            component = "cad-cli",
            operation = "runtime_open_or_create",
            eval_path = "graph_evaluator",
            project = %resolved.display(),
            "opening existing headless .aicad session"
        );
        return runtime
            .open_session(&resolved)
            .map_err(HostError::from_runtime);
    }
    if resolved.exists() {
        return Err(HostError::new(
            "E_IO",
            format!("project path `{}` is not a file", resolved.display()),
        ));
    }
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            HostError::new(
                "E_IO",
                format!("cannot create `{}`: {err}", parent.display()),
            )
        })?;
    }
    tracing::debug!(
        component = "cad-cli",
        operation = "runtime_open_or_create",
        eval_path = "graph_evaluator",
        project = %resolved.display(),
        "creating headless .aicad session"
    );
    runtime
        .create_session(&resolved, project_name(&resolved))
        .map_err(HostError::from_runtime)
}

/// Run one MCP tool against `session` and return the tool JSON payload.
pub(crate) fn mcp_tool_call(
    session: DocumentSession,
    name: &str,
    arguments: Value,
) -> Result<Value, HostError> {
    let mut server = McpServer::from_session(session);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
        }
    });
    let raw = server
        .handle_message(&request.to_string())
        .ok_or_else(|| HostError::new("E_IO", format!("MCP tool `{name}` returned no response")))?;
    parse_mcp_tool_response(name, &raw)
}

fn parse_mcp_tool_response(name: &str, raw: &str) -> Result<Value, HostError> {
    let envelope: Value = serde_json::from_str(raw.trim())
        .map_err(|err| HostError::new("E_IO", format!("MCP response is not JSON: {err}")))?;
    if let Some(error) = envelope.get("error") {
        return Err(HostError::new(
            "E_IO",
            format!("MCP tool `{name}` failed: {error}"),
        ));
    }
    let result = &envelope["result"];
    let text = result["content"]
        .get(0)
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| HostError::new("E_IO", format!("MCP tool `{name}` returned no text")))?;
    let payload: Value = serde_json::from_str(text).map_err(|err| {
        HostError::new(
            "E_IO",
            format!("MCP tool `{name}` payload is not JSON: {err}"),
        )
    })?;
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("E_RUNTIME");
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP tool failed");
        return Err(HostError::new(code, message));
    }
    Ok(payload)
}

fn parse_loopback_endpoint(endpoint: &str) -> Option<(u16, String)> {
    let rest = endpoint.strip_prefix("http://127.0.0.1:")?;
    let (port_text, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if port_text.is_empty() || !port_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port = port_text.parse::<u16>().ok()?;
    if port == 0 || !path.starts_with('/') || !path.bytes().all(|b| matches!(b, b'!'..=b'~')) {
        return None;
    }
    Some((port, path.to_owned()))
}

fn resolve_bearer_token(info: &RuntimeDiscoveryInfo) -> Option<String> {
    if let Some(token) = nonempty_env("AICAD_LIVE_ATTACH_TOKEN") {
        return verified_token(&token, &info.auth_token_sha256);
    }
    let path = token_file_path(&info.auth_token_sha256)?;
    let text = std::fs::read_to_string(path).ok()?;
    let token = text.trim();
    if token.is_empty() {
        None
    } else {
        verified_token(token, &info.auth_token_sha256)
    }
}

fn verified_token(token: &str, expected_sha256: &str) -> Option<String> {
    if token.len() != 64 || !token.is_ascii() {
        return None;
    }
    let bytes = token.as_bytes();
    let mut decoded = Vec::with_capacity(32);
    let mut index = 0;
    while index < bytes.len() {
        let hi = from_hex(bytes[index])?;
        let lo = from_hex(bytes[index + 1])?;
        decoded.push((hi << 4) | lo);
        index += 2;
    }
    if sha256_hex(&decoded).eq_ignore_ascii_case(expected_sha256) {
        Some(token.to_owned())
    } else {
        None
    }
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn token_file_path(auth_token_sha256: &str) -> Option<PathBuf> {
    if auth_token_sha256.len() != 64
        || !auth_token_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let prefix = &auth_token_sha256[..16];
    Some(token_dir()?.join(format!("{prefix}.token")))
}

fn token_dir() -> Option<PathBuf> {
    if let Some(dir) = nonempty_env("AICAD_TOKEN_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        if let Some(local) = nonempty_env("LOCALAPPDATA") {
            return Some(PathBuf::from(local).join("aicad").join("tokens"));
        }
        nonempty_env("USERPROFILE")
            .map(|profile| PathBuf::from(profile).join(".aicad").join("tokens"))
    }
    #[cfg(not(windows))]
    {
        if let Some(runtime) = nonempty_env("XDG_RUNTIME_DIR") {
            return Some(PathBuf::from(runtime).join("aicad").join("tokens"));
        }
        nonempty_env("HOME").map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("aicad")
                .join("tokens")
        })
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn post_json(client: &LiveClient, body: &str) -> std::io::Result<String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, client.port));
    let mut stream = TcpStream::connect_timeout(&addr, LIVE_CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    let header = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Connection: close\r\n\
         Content-Type: application/json\r\n\
         Authorization: Bearer {token}\r\n\
         Origin: http://127.0.0.1:{port}\r\n\
         Content-Length: {length}\r\n\
         \r\n",
        path = client.path,
        port = client.port,
        token = client.token,
        length = body.len(),
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;

    let mut buf = Vec::new();
    let mut tmp = [0_u8; 4096];
    let header_end = loop {
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "live-attach HTTP headers too large",
            ));
        }
        let read = read_some(&mut stream, &mut tmp)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "live-attach closed before HTTP headers",
            ));
        }
        buf.extend_from_slice(&tmp[..read]);
        if let Some(end) = find_header_end(&buf) {
            break end;
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid HTTP header UTF-8")
    })?;
    let status = parse_status(head)?;
    let body_bytes = if let Some(length) = content_length(head) {
        if length > LIVE_MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "live-attach response too large",
            ));
        }
        while buf.len().saturating_sub(header_end) < length {
            let read = read_some(&mut stream, &mut tmp)?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated live-attach response",
                ));
            }
            buf.extend_from_slice(&tmp[..read]);
        }
        &buf[header_end..header_end + length]
    } else {
        loop {
            if buf.len().saturating_sub(header_end) > LIVE_MAX_BODY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "live-attach response too large",
                ));
            }
            let read = read_some(&mut stream, &mut tmp)?;
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..read]);
        }
        &buf[header_end..]
    };
    let text = std::str::from_utf8(body_bytes)
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid HTTP body UTF-8")
        })?
        .to_owned();
    if status != 200 {
        return Err(std::io::Error::other(format!(
            "live-attach returned HTTP {status}: {}",
            text.trim()
        )));
    }
    Ok(text)
}

fn read_some(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .or_else(|| {
            buf.windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| index + 2)
        })
}

fn parse_status(head: &str) -> std::io::Result<u16> {
    let line = head.lines().next().unwrap_or("").trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "live-attach response is not HTTP",
        ));
    }
    parts
        .next()
        .unwrap_or("")
        .parse::<u16>()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "live-attach HTTP status is invalid",
            )
        })
}

fn content_length(head: &str) -> Option<usize> {
    for line in head.lines() {
        let Some((key, value)) = line.trim_end_matches('\r').split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("content-length") {
            return value.trim().parse().ok();
        }
    }
    None
}

fn project_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("project")
        .to_owned()
}

pub(crate) fn display_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

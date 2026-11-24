//! Stdio proxy onto a desktop Live-Attach loopback HTTP host.
//!
//! Discovery matches `apps/desktop/src-tauri/src/live_attach.rs`: the workspace
//! file names `http://127.0.0.1:<port>/...`, and the bearer token is the hex
//! form of the 32 bytes whose SHA-256 is `auth_token_sha256`.

use std::io::{BufRead, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cad_runtime::{
    DISCOVERY_PROTOCOL_VERSION, RuntimeDiscoveryInfo, discovery_pid_is_stale,
    read_runtime_discovery, sha256_hex,
};
use serde_json::Value;

const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const PROBE_BODY: &str = r#"{"jsonrpc":"2.0","id":0,"method":"ping","params":{}}"#;

/// One desktop session this process can forward stdio JSON-RPC to.
pub(crate) struct LiveAttachTarget {
    port: u16,
    path: String,
    token: String,
}

/// Attach when the workspace publishes a live, authenticated loopback host.
///
/// Any failed check returns `None` so the caller can open a headless project.
pub(crate) fn try_attach(workspace: &Path) -> Option<LiveAttachTarget> {
    let info = read_runtime_discovery(workspace).ok()?;
    if info.protocol_version != DISCOVERY_PROTOCOL_VERSION || discovery_pid_is_stale(&info) {
        return None;
    }
    let endpoint = parse_loopback_endpoint(&info.ipc_endpoint)?;
    let token = resolve_bearer_token(&info)?;
    if !probe_healthy(&endpoint, &token) {
        return None;
    }
    Some(LiveAttachTarget {
        port: endpoint.port,
        path: endpoint.path,
        token,
    })
}

/// Forward each stdin JSON-RPC line to the desktop host until stdin reaches EOF.
pub(crate) fn proxy_stdio(target: &LiveAttachTarget) -> std::io::Result<()> {
    eprintln!(
        "cad-cli mcp: live-attach http://127.0.0.1:{}{}",
        target.port, target.path
    );
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let payload = line.trim();
        if payload.is_empty() {
            continue;
        }
        let reply = post_json(target, payload, None)?;
        if let Some(body) = response_body(&reply) {
            writer.write_all(body.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
}

struct LoopbackEndpoint {
    port: u16,
    path: String,
}

/// `AICAD_LIVE_ATTACH_TOKEN` wins when it is non-empty.
/// A value that fails the SHA-256 check rejects attach; the token file is not used.
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
    let bytes = decode_hex(token)?;
    if bytes.len() == 32 && sha256_hex(&bytes).eq_ignore_ascii_case(expected_sha256) {
        Some(token.to_owned())
    } else {
        None
    }
}

fn token_file_path(auth_token_sha256: &str) -> Option<PathBuf> {
    if auth_token_sha256.len() != 64
        || !auth_token_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
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

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(32);
    let mut index = 0;
    while index < bytes.len() {
        let hi = from_hex(bytes[index])?;
        let lo = from_hex(bytes[index + 1])?;
        out.push((hi << 4) | lo);
        index += 2;
    }
    Some(out)
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_loopback_endpoint(endpoint: &str) -> Option<LoopbackEndpoint> {
    let rest = endpoint.strip_prefix("http://127.0.0.1:")?;
    let (port_text, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if port_text.is_empty() || !port_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port = port_text.parse::<u16>().ok()?;
    if port == 0 || !path_is_safe(path) {
        return None;
    }
    Some(LoopbackEndpoint {
        port,
        path: path.to_owned(),
    })
}

fn path_is_safe(path: &str) -> bool {
    path.starts_with('/') && path.bytes().all(|byte| matches!(byte, b'!'..=b'~'))
}

fn probe_healthy(endpoint: &LoopbackEndpoint, token: &str) -> bool {
    let target = LiveAttachTarget {
        port: endpoint.port,
        path: endpoint.path.clone(),
        token: token.to_owned(),
    };
    match post_json(&target, PROBE_BODY, Some(PROBE_TIMEOUT)) {
        Ok(reply) if reply.status == 200 => jsonrpc_result(&reply.body),
        _ => false,
    }
}

fn jsonrpc_result(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body.trim()) else {
        return false;
    };
    value.get("jsonrpc").and_then(Value::as_str) == Some("2.0") && value.get("result").is_some()
}

struct HttpReply {
    status: u16,
    body: String,
}

fn response_body(reply: &HttpReply) -> Option<&str> {
    if reply.status == 202 || reply.status == 204 || reply.body.trim().is_empty() {
        None
    } else {
        Some(reply.body.trim_end_matches(['\r', '\n']))
    }
}

fn post_json(
    target: &LiveAttachTarget,
    body: &str,
    timeout: Option<Duration>,
) -> std::io::Result<HttpReply> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout.unwrap_or(PROXY_CONNECT_TIMEOUT))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let header = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Connection: close\r\n\
         Content-Type: application/json\r\n\
         Authorization: Bearer {token}\r\n\
         Origin: http://127.0.0.1:{port}\r\n\
         Content-Length: {length}\r\n\
         \r\n",
        path = target.path,
        port = target.port,
        token = target.token,
        length = body.len(),
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    read_reply(&mut stream)
}

fn read_reply(stream: &mut TcpStream) -> std::io::Result<HttpReply> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 4096];
    let header_end = loop {
        if buf.len() > 64 * 1024 {
            return Err(invalid_data("live-attach headers are too large"));
        }
        let read = read_some(stream, &mut tmp)?;
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
    let head = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| invalid_data("live-attach headers are not UTF-8"))?;
    let status = parse_status(head)?;
    let Some(length) = content_length(head) else {
        return read_until_close(stream, &mut buf, header_end, status);
    };
    if length > MAX_BODY_BYTES {
        return Err(invalid_data("live-attach response is too large"));
    }
    while buf.len().saturating_sub(header_end) < length {
        let read = read_some(stream, &mut tmp)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated live-attach response",
            ));
        }
        buf.extend_from_slice(&tmp[..read]);
    }
    let body = std::str::from_utf8(&buf[header_end..header_end + length])
        .map_err(|_| invalid_data("live-attach body is not UTF-8"))?
        .to_owned();
    Ok(HttpReply { status, body })
}

fn read_until_close(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    header_end: usize,
    status: u16,
) -> std::io::Result<HttpReply> {
    let mut tmp = [0_u8; 4096];
    loop {
        if buf.len().saturating_sub(header_end) > MAX_BODY_BYTES {
            return Err(invalid_data("live-attach response is too large"));
        }
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(read) => buf.extend_from_slice(&tmp[..read]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    let body = std::str::from_utf8(&buf[header_end..])
        .map_err(|_| invalid_data("live-attach body is not UTF-8"))?
        .to_owned();
    Ok(HttpReply { status, body })
}

fn read_some(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn parse_status(head: &str) -> std::io::Result<u16> {
    let line = head.lines().next().unwrap_or("").trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(invalid_data("live-attach response is not HTTP"));
    }
    parts
        .next()
        .unwrap_or("")
        .parse::<u16>()
        .map_err(|_| invalid_data("live-attach status is invalid"))
}

fn content_length(head: &str) -> Option<usize> {
    header_value(head, "content-length")?.parse().ok()
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    for line in head.lines() {
        let Some((key, value)) = line.trim_end_matches('\r').split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim());
        }
    }
    None
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

fn invalid_data(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

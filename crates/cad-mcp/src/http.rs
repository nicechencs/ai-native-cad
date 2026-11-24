//! Loopback Streamable HTTP guard.
//!
//! The listener binds `127.0.0.1` only. Every request needs an allow-listed
//! `Origin` and `Authorization: Bearer <token>`. Headers are capped at 64 KiB
//! and validated before any request body is read; bodies are capped at 16 MiB
//! and sliced strictly by `Content-Length`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use crate::server::McpServer;

const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Caller-supplied allow list and bearer secret.
#[derive(Debug, Clone)]
pub struct HttpSecurityConfig {
    pub allowed_origins: Vec<String>,
    pub bearer_token: String,
}

impl HttpSecurityConfig {
    /// Maximum allowed HTTP request header section in bytes (64 KiB).
    pub const MAX_HEADER_BYTES: usize = MAX_HTTP_HEADER_BYTES;

    /// Maximum allowed HTTP request body in bytes (16 MiB).
    pub const MAX_BODY_BYTES: usize = MAX_HTTP_BODY_BYTES;

    /// Read and authenticate one bounded HTTP/1.1 `POST` request from `reader`.
    ///
    /// Enforces the 64 KiB header cap (`431`), validates `Authorization: Bearer`
    /// (`401`), `Origin` allow-list (`403`), method `POST` (`405`), and
    /// `Content-Length <= 16 MiB` (`413`) before reading body bytes, then slices
    /// strictly to `header_end + content_length` without consuming trailing bytes.
    ///
    /// Returns `Ok(Ok(body))` for an authorized request body or
    /// `Ok(Err(http_response))` for an immediate HTTP error response.
    pub fn read_authorized_http_body<R: Read>(
        &self,
        reader: &mut R,
    ) -> std::io::Result<Result<String, String>> {
        read_authorized_http_body(reader, self)
    }

    /// Read one bounded HTTP/1.1 request from `stream`, write the response, and
    /// invoke `on_response(server, request_body, response_http)` when an
    /// authorized `POST` body was dispatched.
    pub fn serve_one_http_connection_with_hook<F>(
        &self,
        stream: &mut TcpStream,
        server: &mut McpServer,
        on_response: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce(&mut McpServer, &str, &str),
    {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        match self.read_authorized_http_body(stream)? {
            Ok(body) => {
                let response = dispatch_authorized_body(server, &body);
                stream.write_all(response.as_bytes())?;
                stream.flush()?;
                on_response(server, &body, &response);
                Ok(())
            }
            Err(response) => {
                stream.write_all(response.as_bytes())?;
                stream.flush()?;
                let _ = stream.shutdown(std::net::Shutdown::Write);
                drain_peer_before_close(stream);
                Ok(())
            }
        }
    }
}

fn drain_peer_before_close(stream: &mut TcpStream) {
    // On Windows Winsock, closing a socket with unread bytes in the receive buffer
    // triggers a TCP RST (WSAECONNRESET / 10054) before the client finishes reading
    // an early 401/403/411/413 response. Drain a small bounded amount briefly.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut scratch = [0_u8; 2048];
    let mut drained = 0usize;
    const MAX_DRAIN_BYTES: usize = 64 * 1024;
    while drained < MAX_DRAIN_BYTES {
        match stream.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => drained += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Why a Streamable HTTP request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpAuthError {
    /// Missing or wrong bearer token.
    Unauthorized,
    /// Missing, empty, or non-allow-listed `Origin`.
    ForbiddenOrigin,
}

impl HttpAuthError {
    pub const fn status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::ForbiddenOrigin => 403,
        }
    }

    pub const fn body(self) -> &'static str {
        match self {
            Self::Unauthorized => "{\"error\":\"unauthorized\"}",
            Self::ForbiddenOrigin => "{\"error\":\"forbidden_origin\"}",
        }
    }
}

/// Accept the request only when the bearer token matches and `Origin` is listed.
pub fn validate_http_request(
    config: &HttpSecurityConfig,
    origin: Option<&str>,
    authorization: Option<&str>,
) -> Result<(), HttpAuthError> {
    if !bearer_matches(config, authorization) {
        return Err(HttpAuthError::Unauthorized);
    }
    let Some(origin) = origin.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(HttpAuthError::ForbiddenOrigin);
    };
    if config.allowed_origins.iter().any(|allowed| allowed == origin) {
        Ok(())
    } else {
        Err(HttpAuthError::ForbiddenOrigin)
    }
}

/// Bind the HTTP transport to an ephemeral port on `127.0.0.1`.
pub fn bind_streamable_http() -> std::io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
}

/// Read one HTTP/1.1 request from `stream` and write one response.
pub fn serve_one_http_connection(
    stream: &mut TcpStream,
    server: &mut McpServer,
    config: &HttpSecurityConfig,
) -> std::io::Result<()> {
    config.serve_one_http_connection_with_hook(stream, server, |_, _, _| {})
}

/// Parsed HTTP result used by the loopback listener.
pub fn handle_http_request(
    server: &mut McpServer,
    config: &HttpSecurityConfig,
    raw: &str,
) -> String {
    let raw_bytes = raw.as_bytes();
    let (head_bytes, body_bytes) = match find_header_end(raw_bytes) {
        Some(header_end) => {
            if header_end > MAX_HTTP_HEADER_BYTES {
                return http_response(431, "{\"error\":\"request_header_fields_too_large\"}");
            }
            (&raw_bytes[..header_end], &raw_bytes[header_end..])
        }
        None => {
            if raw_bytes.len() > MAX_HTTP_HEADER_BYTES {
                return http_response(431, "{\"error\":\"request_header_fields_too_large\"}");
            }
            (raw_bytes, &b""[..])
        }
    };
    let head = String::from_utf8_lossy(head_bytes);
    let expected_len = match validate_headers(config, &head) {
        Ok(len) => len,
        Err(response) => return response,
    };
    let sliced = match expected_len {
        Some(len) => {
            if body_bytes.len() < len {
                return http_response(400, "{\"error\":\"incomplete_body\"}");
            }
            &body_bytes[..len]
        }
        None => {
            if body_bytes.len() > MAX_HTTP_BODY_BYTES {
                return http_response(413, "{\"error\":\"payload_too_large\"}");
            }
            body_bytes
        }
    };
    let body = String::from_utf8_lossy(sliced);
    dispatch_authorized_body(server, &body)
}

fn dispatch_authorized_body(server: &mut McpServer, body: &str) -> String {
    let payload = body.trim();
    if payload.is_empty() {
        return http_response(400, "{\"error\":\"empty_body\"}");
    }
    match server.handle_message(payload) {
        Some(json) => http_response(200, &json),
        None => http_response(202, ""),
    }
}

fn validate_headers(config: &HttpSecurityConfig, head: &str) -> Result<Option<usize>, String> {
    let origin = header_value(head, "origin");
    let authorization = header_value(head, "authorization");
    if let Err(err) = validate_http_request(config, origin.as_deref(), authorization.as_deref()) {
        return Err(http_response(err.status(), err.body()));
    }
    let method = head.split_whitespace().next().unwrap_or("");
    if !method.eq_ignore_ascii_case("POST") {
        return Err(http_response(405, "{\"error\":\"method_not_allowed\"}"));
    }
    if header_value(head, "transfer-encoding").is_some() {
        return Err(http_response(
            400,
            "{\"error\":\"unsupported_transfer_encoding\"}",
        ));
    }
    match parse_content_length(head) {
        ContentLength::Missing => Ok(None),
        ContentLength::Invalid => Err(http_response(400, "{\"error\":\"invalid_content_length\"}")),
        ContentLength::Valid(len) if len > MAX_HTTP_BODY_BYTES => {
            Err(http_response(413, "{\"error\":\"payload_too_large\"}"))
        }
        ContentLength::Valid(len) => Ok(Some(len)),
    }
}

fn read_authorized_http_body<R: Read>(
    stream: &mut R,
    config: &HttpSecurityConfig,
) -> std::io::Result<Result<String, String>> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 2048];
    let header_end = loop {
        if let Some(end) = find_header_end(&buf) {
            if end > MAX_HTTP_HEADER_BYTES {
                return Ok(Err(http_response(
                    431,
                    "{\"error\":\"request_header_fields_too_large\"}",
                )));
            }
            break end;
        }
        if buf.len() >= MAX_HTTP_HEADER_BYTES {
            return Ok(Err(http_response(
                431,
                "{\"error\":\"request_header_fields_too_large\"}",
            )));
        }
        let max_read = (MAX_HTTP_HEADER_BYTES - buf.len() + 4).min(tmp.len());
        let n = match stream.read(&mut tmp[..max_read]) {
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if n == 0 {
            let head = String::from_utf8_lossy(&buf);
            if let Err(response) = validate_headers(config, &head) {
                return Ok(Err(response));
            }
            return Ok(Err(http_response(400, "{\"error\":\"empty_body\"}")));
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let expected_len = match validate_headers(config, &head) {
        Ok(Some(len)) => len,
        Ok(None) => {
            return Ok(Err(http_response(411, "{\"error\":\"length_required\"}")));
        }
        Err(response) => return Ok(Err(response)),
    };

    let Some(target_total) = header_end.checked_add(expected_len) else {
        return Ok(Err(http_response(413, "{\"error\":\"payload_too_large\"}")));
    };

    while buf.len() < target_total {
        let to_read = (target_total - buf.len()).min(tmp.len());
        let n = match stream.read(&mut tmp[..to_read]) {
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if n == 0 {
            return Ok(Err(http_response(400, "{\"error\":\"incomplete_body\"}")));
        }
        buf.extend_from_slice(&tmp[..n]);
    }

    let body = String::from_utf8_lossy(&buf[header_end..target_total]).into_owned();
    Ok(Ok(body))
}

fn bearer_matches(config: &HttpSecurityConfig, authorization: Option<&str>) -> bool {
    let Some(header) = authorization.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let mut parts = header.splitn(2, char::is_whitespace);
    let scheme = parts.next().unwrap_or("");
    let token = parts.next().unwrap_or("").trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return false;
    }
    tokens_match(config.bearer_token.as_bytes(), token.as_bytes())
}

fn tokens_match(expected: &[u8], presented: &[u8]) -> bool {
    if expected.is_empty() || presented.is_empty() {
        return false;
    }
    let expected_digest = cad_runtime::sha256_hex(expected);
    let presented_digest = cad_runtime::sha256_hex(presented);
    let left = expected_digest.as_bytes();
    let right = presented_digest.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn header_value(head: &str, name: &str) -> Option<String> {
    for line in head.lines().skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim().to_owned());
        }
    }
    None
}

enum ContentLength {
    Missing,
    Valid(usize),
    Invalid,
}

fn parse_content_length(head: &str) -> ContentLength {
    let mut parsed: Option<usize> = None;
    for line in head.lines().skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        let Ok(len) = value.trim().parse::<usize>() else {
            return ContentLength::Invalid;
        };
        if let Some(prev) = parsed {
            if prev != len {
                return ContentLength::Invalid;
            }
        } else {
            parsed = Some(len);
        }
    }
    match parsed {
        Some(len) => ContentLength::Valid(len),
        None => ContentLength::Missing,
    }
}

fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    let crlf = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4);
    let lf = buf
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| index + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

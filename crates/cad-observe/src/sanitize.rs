//! Path anonymisation, message truncation and secret redaction.

use std::borrow::Cow;
use std::path::{Component, Path};

/// Maximum message length in characters.
pub const MAX_MESSAGE_CHARS: usize = 512;

const SECRET_KEYS: [&str; 8] = [
    "api_key",
    "apikey",
    "api-key",
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
];

fn strip_windows_verbatim_prefix(text: &str) -> Cow<'_, str> {
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(r"UNC\") {
            return Cow::Owned(format!(r"\\{}", &rest[4..]));
        }
        return Cow::Borrowed(rest);
    }
    Cow::Borrowed(text)
}

fn is_windows_drive_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_rooted(text: &str) -> bool {
    is_windows_drive_prefix(text) || text.starts_with("\\\\")
}

fn has_windows_parent_traversal(text: &str) -> bool {
    text.contains('\\') && text.split(['/', '\\']).any(|part| part == "." || part == "..")
}

/// Strip a Windows `workspace_root` prefix case-insensitively on any host OS.
fn strip_windows_workspace_prefix(path: &str, root: &str) -> Option<String> {
    if !is_windows_rooted(path) || !is_windows_rooted(root) {
        return None;
    }
    let path_parts: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let root_parts: Vec<&str> = root.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    if root_parts.is_empty() || path_parts.len() <= root_parts.len() {
        return None;
    }
    if !path_parts[..root_parts.len()]
        .iter()
        .zip(root_parts.iter())
        .all(|(p, r)| p.eq_ignore_ascii_case(r))
    {
        return None;
    }
    let rel = &path_parts[root_parts.len()..];
    if rel.iter().any(|part| *part == "." || *part == "..") {
        return None;
    }
    Some(rel.join("/"))
}

fn external_basename(text: &str) -> String {
    let base = text
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("unknown");
    format!("<external>/{base}")
}

/// Workspace-relative path with `/` separators; paths outside the workspace
/// (or when no workspace is known) become `<external>/<basename>`.
pub fn sanitize_path(path: impl AsRef<Path>, workspace_root: Option<&Path>) -> String {
    let rendered = path.as_ref().to_string_lossy();
    let normalized = strip_windows_verbatim_prefix(rendered.trim());
    let trimmed = normalized.as_ref();
    let path = Path::new(trimmed);
    if let Some(root) = workspace_root {
        let root_rendered = root.to_string_lossy();
        let root_normalized = strip_windows_verbatim_prefix(root_rendered.trim());
        let root_trimmed = root_normalized.as_ref();
        let root_path = Path::new(root_trimmed);
        if let Ok(rel) = path.strip_prefix(root_path)
            && !rel.as_os_str().is_empty()
            && rel.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
        }
        if let Some(rel) = strip_windows_workspace_prefix(trimmed, root_trimmed) {
            return rel;
        }
    }
    // Windows drive/UNC paths and backslash parent traversals look like single
    // relative components on Unix, so reject them before the relative-path shortcut.
    if is_windows_rooted(trimmed) || has_windows_parent_traversal(trimmed) {
        return external_basename(trimmed);
    }
    if path.is_relative()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && path.components().next().is_some()
    {
        return path
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
    }
    // Handle Windows separators even when running elsewhere.
    external_basename(trimmed)
}

/// Truncate to at most `max_chars` characters, appending `…` when cut.
pub fn truncate_message(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_owned();
    }
    let mut out: String = message.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '=')
}

/// Redact `key=value` / `key: value` secrets, `Bearer` tokens and `sk-` keys.
pub fn redact_secrets(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let bytes = message.as_bytes();
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < message.len() {
        if !message.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &lower[i..];
        let mut matched = None;
        for key in SECRET_KEYS {
            if rest.starts_with(key) {
                let mut j = i + key.len();
                while j < bytes.len() && matches!(bytes[j], b'"' | b'\'' | b' ') {
                    j += 1;
                }
                if j < bytes.len() && matches!(bytes[j], b'=' | b':') {
                    j += 1;
                    while j < bytes.len() && matches!(bytes[j], b'"' | b'\'' | b' ') {
                        j += 1;
                    }
                    let end = message[j..]
                        .find(|c: char| {
                            c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '}')
                        })
                        .map_or(message.len(), |n| j + n);
                    matched = Some((j, end));
                    break;
                }
            }
        }
        if matched.is_none() && rest.starts_with("bearer ") {
            let j = i + 7;
            let end = message[j..]
                .find(|c: char| !is_token_char(c))
                .map_or(message.len(), |n| j + n);
            matched = Some((j, end));
        }
        if matched.is_none() && rest.starts_with("sk-") {
            let end = message[i..]
                .find(|c: char| !is_token_char(c))
                .map_or(message.len(), |n| i + n);
            if end - i >= 12 {
                out.push_str("<redacted>");
                i = end;
                continue;
            }
        }
        if let Some((value_start, end)) = matched {
            out.push_str(&message[i..value_start]);
            out.push_str("<redacted>");
            i = end.max(value_start);
            continue;
        }
        let ch = message[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Redact secrets, then truncate to [`MAX_MESSAGE_CHARS`]. Newlines collapse to spaces.
pub fn sanitize_message(message: &str) -> String {
    let flat = message.replace(['\r', '\n'], " ");
    truncate_message(&redact_secrets(&flat), MAX_MESSAGE_CHARS)
}

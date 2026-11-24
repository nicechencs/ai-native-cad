//! `.aicad-runtime.json` auto-attach record.
//!
//! The file holds the process id, a nonce, the IPC endpoint, and the SHA-256
//! of the bearer token. The raw token is returned to the host and is not written
//! into the workspace.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::RuntimeError;

/// File name written at the workspace root.
pub const RUNTIME_DISCOVERY_FILE: &str = ".aicad-runtime.json";

/// Schema version stored in [`RuntimeDiscoveryInfo::protocol_version`].
pub const DISCOVERY_PROTOCOL_VERSION: u32 = 1;

const TEMP_SUFFIX: &str = ".aicad-runtime.json.tmp";

/// Probe record an external agent reads before attaching to a live host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDiscoveryInfo {
    pub protocol_version: u32,
    pub pid: u32,
    pub instance_nonce: String,
    pub ipc_endpoint: String,
    pub auth_token_sha256: String,
    pub active_project: String,
}

/// Discovery file plus the one-time bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedDiscovery {
    pub info: RuntimeDiscoveryInfo,
    /// 32 random bytes. Only their SHA-256 is stored in the JSON file.
    pub auth_token: [u8; 32],
}

/// Lower-case SHA-256 hex digest.
pub fn sha256_hex(bytes: &[u8]) -> String {
    cad_container::compute_sha256_hex(bytes)
}

/// Default local IPC name for this workspace. The desktop host binds it later.
pub fn default_ipc_endpoint(workspace_root: &Path) -> String {
    let identity = workspace_root.display().to_string();
    let hash = sha256_hex(identity.as_bytes());
    let prefix = &hash[..16];
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\aicad-rt-{prefix}")
    }
    #[cfg(not(windows))]
    {
        format!("aicad-rt-{prefix}.sock")
    }
}

/// Create a fresh discovery record and write it atomically.
pub fn publish_runtime_discovery(
    workspace_root: &Path,
    ipc_endpoint: &str,
    active_project: &str,
) -> Result<PublishedDiscovery, RuntimeError> {
    let mut auth_token = [0u8; 32];
    let mut nonce = [0u8; 16];
    fill_random(&mut auth_token)?;
    fill_random(&mut nonce)?;
    let info = RuntimeDiscoveryInfo {
        protocol_version: DISCOVERY_PROTOCOL_VERSION,
        pid: std::process::id(),
        instance_nonce: hex_encode(&nonce),
        ipc_endpoint: ipc_endpoint.to_owned(),
        auth_token_sha256: sha256_hex(&auth_token),
        active_project: active_project.to_owned(),
    };
    write_runtime_discovery(workspace_root, &info)?;
    Ok(PublishedDiscovery { info, auth_token })
}

/// Write `info` via a temp file, then replace `.aicad-runtime.json`.
pub fn write_runtime_discovery(
    workspace_root: &Path,
    info: &RuntimeDiscoveryInfo,
) -> Result<(), RuntimeError> {
    if !workspace_root.is_dir() {
        return Err(RuntimeError::Discovery(format!(
            "workspace root `{}` is not a directory",
            workspace_root.display()
        )));
    }
    let dest = discovery_path(workspace_root);
    let body = serde_json::to_vec_pretty(info)
        .map_err(|err| RuntimeError::Discovery(err.to_string()))?;
    crate::write_file_atomic(&dest, &body)?;
    Ok(())
}

/// Read and parse `.aicad-runtime.json`.
pub fn read_runtime_discovery(workspace_root: &Path) -> Result<RuntimeDiscoveryInfo, RuntimeError> {
    let text = std::fs::read_to_string(discovery_path(workspace_root))?;
    serde_json::from_str(&text).map_err(|err| RuntimeError::Discovery(err.to_string()))
}

/// Remove the discovery file. Missing files are not an error.
pub fn cleanup_runtime_discovery(workspace_root: &Path) -> Result<(), RuntimeError> {
    remove_if_present(&discovery_path(workspace_root))?;
    remove_if_present(&workspace_root.join(TEMP_SUFFIX))?;
    Ok(())
}

/// `true` when `info.pid` is not a running process.
///
/// The current process is never stale. A pid of `0` is always stale.
pub fn discovery_pid_is_stale(info: &RuntimeDiscoveryInfo) -> bool {
    !process_is_alive(info.pid)
}

fn discovery_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(RUNTIME_DISCOVERY_FILE)
}

fn remove_if_present(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn fill_random(buf: &mut [u8]) -> Result<(), RuntimeError> {
    getrandom::fill(buf).map_err(|err| RuntimeError::Discovery(err.to_string()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    #[cfg(windows)]
    {
        windows_pid_alive(pid)
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        unix_ps_alive(pid)
    }
}

#[cfg(windows)]
fn windows_pid_alive(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            csv_contains_pid(&String::from_utf8_lossy(&output.stdout), pid)
        }
        _ => false,
    }
}

#[cfg(windows)]
fn csv_contains_pid(text: &str, pid: u32) -> bool {
    let needle = pid.to_string();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line
            .split(',')
            .map(|field| field.trim().trim_matches('"'))
            .collect();
        if fields.get(1).is_some_and(|field| *field == needle) {
            return true;
        }
    }
    false
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_ps_alive(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        }
        _ => false,
    }
}

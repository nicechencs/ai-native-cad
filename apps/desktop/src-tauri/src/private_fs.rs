//! Unified private file/directory ACL helpers and Windows verbatim path cleanup.
//!
//! Shared by `ai_bridge` (API key profile store) and `live_attach` (loopback
//! MCP bearer token storage) so secret files on disk are always restricted to
//! the current user on both Unix (`0o700`/`0o600`) and Windows (protected DACL
//! granting access only to the current user SID via `icacls.exe`).

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Strip Windows verbatim path prefixes (`\\?\UNC\` -> `\\`, `\\?\` -> ``).
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let stripped = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| text.into_owned());
    PathBuf::from(stripped)
}

/// Return `true` when `meta` describes a symlink or (on Windows) a reparse point.
pub fn is_symlink_or_reparse_point(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Reject an existing symlink or Windows reparse point at `path`.
pub fn reject_symlink_path(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if is_symlink_or_reparse_point(&meta) => Err(format!(
            "E_IO: refusing to use symlinked {label} `{}`",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("E_IO: {err}")),
    }
}

/// Create `dir` (including missing parents) and restrict it to the current user.
pub fn create_private_dir(dir: &Path, label: &str) -> Result<(), String> {
    reject_symlink_path(dir, label)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        builder.mode(0o700);
        builder
            .create(dir)
            .map_err(|err| format!("E_IO: failed to create {label}: {err}"))?;
        let mut perms = fs::metadata(dir)
            .map_err(|err| format!("E_IO: failed to read {label}: {err}"))?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir, perms)
            .map_err(|err| format!("E_IO: failed to restrict {label}: {err}"))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(dir)
            .map_err(|err| format!("E_IO: failed to create {label}: {err}"))?;
        reject_symlink_path(dir, label)?;
        restrict_windows_acl(dir, true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::create_dir_all(dir).map_err(|err| format!("E_IO: failed to create {label}: {err}"))
    }
}

/// Write `bytes` to `path` and restrict permissions/ACL to the current user.
pub fn write_private_file(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    reject_symlink_path(path, label)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|err| format!("E_IO: failed to write {label}: {err}"))?;
        file.write_all(bytes)
            .map_err(|err| format!("E_IO: failed to write {label}: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("E_IO: failed to sync {label}: {err}"))?;
        drop(file);
        let mut perms = fs::metadata(path)
            .map_err(|err| format!("E_IO: failed to read {label}: {err}"))?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)
            .map_err(|err| format!("E_IO: failed to restrict {label}: {err}"))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|err| format!("E_IO: failed to write {label}: {err}"))?;
        file.write_all(bytes)
            .map_err(|err| format!("E_IO: failed to write {label}: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("E_IO: failed to sync {label}: {err}"))?;
        drop(file);
        restrict_windows_acl(path, false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|err| format!("E_IO: failed to write {label}: {err}"))?;
        file.write_all(bytes)
            .map_err(|err| format!("E_IO: failed to write {label}: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("E_IO: failed to sync {label}: {err}"))?;
        Ok(())
    }
}

/// Return `true` when `path` is restricted exclusively to the current user.
pub fn path_is_private(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|metadata| {
                let expected = if metadata.is_dir() { 0o700 } else { 0o600 };
                metadata.permissions().mode() & 0o777 == expected
            })
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        windows_acl_is_private(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.exists()
    }
}

#[cfg(windows)]
#[derive(Clone, Debug)]
struct WindowsUserIdentity {
    account: String,
    sid: String,
}

#[cfg(windows)]
fn windows_system32_exe(name: &str) -> PathBuf {
    if let Ok(root) = std::env::var("SystemRoot").or_else(|_| std::env::var("WINDIR")) {
        let candidate = PathBuf::from(root).join("System32").join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

#[cfg(windows)]
fn current_windows_user_identity() -> Result<WindowsUserIdentity, String> {
    use std::os::windows::process::CommandExt;
    use std::sync::{Mutex, OnceLock};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    static IDENTITY_CACHE: OnceLock<Mutex<Option<WindowsUserIdentity>>> = OnceLock::new();
    let cache = IDENTITY_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some(identity) = guard.as_ref()
    {
        return Ok(identity.clone());
    }
    let output = std::process::Command::new(windows_system32_exe("whoami.exe"))
        .args(["/user", "/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| format!("E_IO: failed to resolve current Windows user SID: {err}"))?;
    if !output.status.success() {
        return Err("E_IO: whoami.exe failed while resolving current Windows user SID".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((account_part, sid_part)) = trimmed.rsplit_once(',') {
            let account = account_part.trim().trim_matches('"').to_owned();
            let sid = sid_part.trim().trim_matches('"');
            if !account.is_empty()
                && sid.starts_with("S-1-")
                && sid
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            {
                let identity = WindowsUserIdentity {
                    account,
                    sid: sid.to_owned(),
                };
                if let Ok(mut guard) = cache.lock() {
                    *guard = Some(identity.clone());
                }
                return Ok(identity);
            }
        }
    }
    Err("E_IO: could not parse current Windows user SID from whoami output".into())
}

#[cfg(windows)]
fn restrict_windows_acl(path: &Path, is_dir: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let identity = current_windows_user_identity()?;
    let grant = if is_dir {
        format!("*{}:(OI)(CI)F", identity.sid)
    } else {
        format!("*{}:F", identity.sid)
    };
    let clean_path = strip_verbatim_prefix(path);
    let output = std::process::Command::new(windows_system32_exe("icacls.exe"))
        .arg(&clean_path)
        .args(["/inheritance:r", "/grant:r", &grant])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| {
            format!(
                "E_IO: failed to tighten Windows ACL on `{}`: {err}",
                clean_path.display()
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let fallback = String::from_utf8_lossy(&output.stdout);
        let message = if detail.trim().is_empty() {
            fallback.trim()
        } else {
            detail.trim()
        };
        return Err(format!(
            "E_IO: icacls failed on `{}`: {message}",
            clean_path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_acl_is_private(path: &Path) -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(identity) = current_windows_user_identity() else {
        return false;
    };
    let clean_path = strip_verbatim_prefix(path);
    let Ok(output) = std::process::Command::new(windows_system32_exe("icacls.exe"))
        .arg(&clean_path)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path_str = clean_path.to_string_lossy();
    icacls_output_is_private(&stdout, &path_str, &identity.account, &identity.sid)
}

#[cfg(any(windows, test))]
fn icacls_output_is_private(output: &str, path_str: &str, account: &str, sid: &str) -> bool {
    let mut ace_count = 0usize;
    let path_lower = path_str.to_ascii_lowercase();
    let star_sid = format!("*{sid}");
    for (idx, raw_line) in output.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            break;
        }
        let ace = if idx == 0 {
            let line_lower = line.to_ascii_lowercase();
            if line_lower.starts_with(&path_lower) {
                line[path_str.len()..].trim()
            } else {
                line.trim()
            }
        } else {
            line.trim()
        };
        if ace.is_empty() {
            continue;
        }
        let Some((principal, perms)) = ace.split_once(':') else {
            return false;
        };
        let principal = principal.trim();
        let perms = perms.trim();
        if perms.contains("(I)") {
            return false;
        }
        let matches_user = principal.eq_ignore_ascii_case(account)
            || principal.eq_ignore_ascii_case(sid)
            || principal.eq_ignore_ascii_case(&star_sid);
        if !matches_user {
            return false;
        }
        ace_count += 1;
    }
    ace_count > 0
}

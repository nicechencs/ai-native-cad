//! Crash-safe atomic file writes via same-directory staging and rename.
//!
//! Writes bytes to a unique temporary file in the target's parent directory,
//! flushes it with `sync_all`, closes the handle, and replaces the destination
//! with `std::fs::rename`. If any step fails, the temporary file is removed
//! automatically and any pre-existing destination file remains intact.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TEMP_ATTEMPTS: usize = 64;

struct TempFileGuard {
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Write `bytes` to `target` atomically via a same-directory temporary file.
///
/// Creates missing parent directories automatically. On failure, any temporary
/// staging file is cleaned up and an existing file at `target` is left untouched.
pub fn write_file_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if target.as_os_str().is_empty() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "target path must not be empty",
        ));
    }
    if target.is_dir() {
        return Err(io::Error::new(
            ErrorKind::IsADirectory,
            format!("target path `{}` is a directory", target.display()),
        ));
    }
    let parent = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let (mut file, tmp_path) = create_unique_temp_file(parent, target)?;
    let mut guard = TempFileGuard::new(tmp_path.clone());

    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    replace_file(&tmp_path, target)?;
    guard.disarm();
    Ok(())
}

fn create_unique_temp_file(parent: &Path, target: &Path) -> io::Result<(fs::File, PathBuf)> {
    static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);
    let stem = target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("export");
    let pid = std::process::id();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_nanos())
        .unwrap_or(0);

    for _ in 0..MAX_TEMP_ATTEMPTS {
        let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{stem}.tmp.{pid}.{stamp:x}.{seq}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, candidate)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        format!(
            "could not allocate unique staging file in `{}`",
            parent.display()
        ),
    ))
}

fn replace_file(tmp_path: &Path, target: &Path) -> io::Result<()> {
    match fs::rename(tmp_path, target) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(err) if err.kind() == ErrorKind::AlreadyExists && target.is_file() => {
            replace_existing_windows_fallback(tmp_path, target)
        }
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
fn replace_existing_windows_fallback(tmp_path: &Path, target: &Path) -> io::Result<()> {
    static NEXT_BACKUP: AtomicU64 = AtomicU64::new(0);
    let parent = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("export");
    let seq = NEXT_BACKUP.fetch_add(1, Ordering::Relaxed);
    let backup = parent.join(format!(".{stem}.bak.{}.{seq}", std::process::id()));

    fs::rename(target, &backup)?;
    match fs::rename(tmp_path, target) {
        Ok(()) => {
            let _ = fs::remove_file(&backup);
            Ok(())
        }
        Err(err) => {
            let _ = fs::rename(&backup, target);
            Err(err)
        }
    }
}

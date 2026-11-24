//! Deterministic container format sniffing, reparse-point guards, and network journal policies.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, Prefix},
};

use serde::{Deserialize, Serialize};

use crate::ContainerError;

/// Standard 16-byte SQLite file header magic (`"SQLite format 3\0"`).
pub const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Registered SQLite `application_id` for `.aicad` containers (ASCII `"AICD"` = `0x41494344`).
pub const SQLITE_APPLICATION_ID: i32 = 0x4149_4344;

/// Physical storage backend detected for an `.aicad` project path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    UnpackedSingleGraphDir,
    UnpackedShardedDir,
    SqliteContainer,
}

impl StorageKind {
    /// Human-readable identifier used in reports and bundled reference metadata.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnpackedSingleGraphDir => "single_dir",
            Self::UnpackedShardedDir => "sharded_dir",
            Self::SqliteContainer => "sqlite",
        }
    }
}

/// Transaction journal strategy for SQLite `.aicad` containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPolicy {
    /// Use `WAL` on local disks and automatically fall back to `DELETE` rollback journal on UNC/network paths.
    #[default]
    Auto,
    /// Explicitly request `WAL` mode (still falls back to `DELETE` if the path is a UNC network share).
    Wal,
    /// Force `DELETE` rollback journal with `FULL` synchronous durability (safe on SMB/NFS/NAS shares).
    SafeRollback,
}

/// Returns true if `metadata` represents a symbolic link or Windows Reparse Point / Junction.
pub fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_type().is_symlink()
            || (metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Returns true when `path` points to a Windows UNC / network share or safe-journal override is set.
pub fn is_network_or_unc_path(path: &Path) -> bool {
    if std::env::var_os("AICAD_SQLITE_SAFE_JOURNAL")
        .is_some_and(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    {
        return true;
    }

    for component in path.components() {
        if let Component::Prefix(prefix) = component
            && matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))
        {
            return true;
        }
    }

    let raw = path.to_string_lossy();
    if raw.len() >= 8 && raw[..8].eq_ignore_ascii_case(r"\\?\UNC\") {
        return true;
    }
    if raw.starts_with(r"\\") && !raw.starts_with(r"\\?\") && !raw.starts_with(r"\\.\") {
        return true;
    }
    if raw.starts_with("//")
        && !raw.starts_with("///")
        && !raw.starts_with("//?/")
        && !raw.starts_with("//./")
    {
        return true;
    }

    false
}

/// Determine the target SQLite `journal_mode` (`"WAL"` or `"DELETE"`) for `path` and `policy`.
pub fn resolve_journal_mode_for_path(path: &Path, policy: JournalPolicy) -> &'static str {
    match policy {
        JournalPolicy::SafeRollback => "DELETE",
        JournalPolicy::Auto | JournalPolicy::Wal => {
            if is_network_or_unc_path(path) {
                "DELETE"
            } else {
                "WAL"
            }
        }
    }
}

/// Detect the physical storage backend of `path` after enforcing symlink / reparse-point safety.
pub fn detect_storage_kind(path: &Path) -> Result<StorageKind, ContainerError> {
    let metadata = fs::symlink_metadata(path)?;
    if is_reparse_or_symlink(&metadata) {
        return Err(ContainerError::UnsafePath(path.display().to_string()));
    }

    if metadata.is_dir() {
        let manifest_path = path.join("manifest.json");
        let manifest_meta = match fs::symlink_metadata(&manifest_path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(ContainerError::MissingEntry("manifest.json".to_owned()));
            }
            Err(err) => return Err(err.into()),
        };
        if is_reparse_or_symlink(&manifest_meta) || !manifest_meta.is_file() {
            return Err(ContainerError::UnsafePath("manifest.json".to_owned()));
        }

        let graphs_dir = path.join("graphs");
        match fs::symlink_metadata(&graphs_dir) {
            Ok(graphs_meta) => {
                if is_reparse_or_symlink(&graphs_meta) || !graphs_meta.is_dir() {
                    return Err(ContainerError::UnsafePath("graphs".to_owned()));
                }
                let mut has_ir_shard = false;
                for child in fs::read_dir(&graphs_dir)? {
                    let child = child?;
                    let child_meta = fs::symlink_metadata(child.path())?;
                    if is_reparse_or_symlink(&child_meta) {
                        return Err(ContainerError::UnsafePath(
                            child.path().display().to_string(),
                        ));
                    }
                    if child_meta.is_file()
                        && child
                            .file_name()
                            .to_str()
                            .is_some_and(|name| name.ends_with(".ir.json"))
                    {
                        has_ir_shard = true;
                    }
                }
                if has_ir_shard {
                    return Ok(StorageKind::UnpackedShardedDir);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }

        return Ok(StorageKind::UnpackedSingleGraphDir);
    }

    if metadata.is_file() {
        let mut file = File::open(path)?;
        let mut magic = [0_u8; 16];
        let read_len = read_up_to(&mut file, &mut magic)?;

        if read_len == 16 && &magic == SQLITE_MAGIC {
            return Ok(StorageKind::SqliteContainer);
        }
        if read_len >= 4 && matches!(&magic[..4], b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08") {
            return Err(ContainerError::UnsupportedFormat(format!(
                "{}: legacy ZIP .aicad archives are retired; only SQLite and directory/sharded-directory containers are supported",
                path.display()
            )));
        }

        return Err(ContainerError::UnsupportedFormat(
            path.display().to_string(),
        ));
    }

    Err(ContainerError::UnsafePath(path.display().to_string()))
}

fn read_up_to(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(total)
}

//! Workspace-relative CAD asset lookup. Importers must consume the returned bytes, not reopen an
//! asset path: only those bytes have passed the resolver checks and can be hashed or cached.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Maximum raw file size accepted by the filesystem resolver (128 MiB). This is not a bound on
/// decoded mesh size or native parser memory; those limits belong to the importers.
pub const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// Host-supplied source of owned, workspace-relative asset bytes.
pub trait AssetResolver {
    fn resolve(&self, path: &str) -> Result<Vec<u8>, AssetResolveError>;
}

#[derive(Debug, thiserror::Error)]
pub enum AssetResolveError {
    #[error("configure a workspace root to import assets")]
    NoWorkspace,
    #[error("asset is unavailable: {0}")]
    Io(#[from] io::Error),
    #[error("unsafe asset path: {0}")]
    UnsafePath(&'static str),
    #[error("asset exceeds the {limit}-byte file size limit")]
    TooLarge { limit: u64 },
    #[error("asset SHA-256 mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
}

impl AssetResolveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoWorkspace | Self::Io(_) | Self::TooLarge { .. } => "E501_ASSET_NOT_FOUND",
            Self::UnsafePath(_) => "E502_UNSAFE_ASSET_PATH",
            Self::HashMismatch { .. } => "E503_ASSET_HASH_MISMATCH",
        }
    }
}

#[derive(Debug, Default)]
pub struct NoopAssetResolver;

impl AssetResolver for NoopAssetResolver {
    fn resolve(&self, _path: &str) -> Result<Vec<u8>, AssetResolveError> {
        Err(AssetResolveError::NoWorkspace)
    }
}

/// Resolves files inside a trusted, host-selected workspace directory.
///
/// Containment here concerns filesystem paths, not file provenance: existing hard links and
/// mounts within the workspace can refer to data originating elsewhere. The host should keep the
/// root and its ancestors stable. Concurrent writers can still change Linux/macOS file contents
/// while they are being read; the bytes returned (and hashed) are not a filesystem snapshot.
/// Linux requires `openat2` (kernel 5.6+); macOS and Windows require a verifiable opened-handle
/// path. Other platforms fail closed rather than use an unchecked path-based read.
#[derive(Debug)]
pub struct FsAssetResolver {
    root: PathBuf,
    #[cfg(target_os = "linux")]
    root_dir: File,
}

impl FsAssetResolver {
    pub fn new(workspace_root: PathBuf) -> Result<Self, AssetResolveError> {
        let root = fs::canonicalize(workspace_root)?;
        #[cfg(target_os = "linux")]
        let root_dir = {
            use std::os::unix::fs::OpenOptionsExt;

            // O_PATH pins the directory for openat2 without requiring permission to read its list
            // of entries; O_NOFOLLOW guards the final component between canonicalize and open.
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(&root)?
        };
        #[cfg(not(target_os = "linux"))]
        if !fs::metadata(&root)?.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "workspace root").into());
        }
        Ok(Self {
            root,
            #[cfg(target_os = "linux")]
            root_dir,
        })
    }

    #[cfg(target_os = "linux")]
    fn open_checked(&self, canonical: &Path) -> Result<File, AssetResolveError> {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;

        let relative = canonical.strip_prefix(&self.root).map_err(|_| outside())?;
        let relative = CString::new(relative.as_os_str().as_bytes())
            .map_err(|_| AssetResolveError::UnsafePath("path contains a null byte"))?;
        // SAFETY: open_how is a repr(C) struct of u64 fields; zero sets its unused mode to 0.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64;
        // We already resolved links in `canonical`. Forbid any new links during the actual open,
        // and have the kernel enforce containment relative to the pinned root directory.
        how.resolve =
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
        // SAFETY: the root fd is live, relative is NUL-terminated, and `how` has the kernel ABI.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.root_dir.as_raw_fd(),
                relative.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EAGAIN | libc::ELOOP | libc::EXDEV) => Err(outside()),
                Some(libc::ENOSYS) => Err(AssetResolveError::UnsafePath(
                    "cannot verify containment: Linux openat2 is unavailable",
                )),
                _ => Err(error.into()),
            };
        }
        // SAFETY: openat2 returned a new owned file descriptor after a successful open.
        Ok(unsafe { File::from_raw_fd(fd as _) })
    }

    #[cfg(target_os = "macos")]
    fn open_checked(&self, canonical: &Path) -> Result<File, AssetResolveError> {
        use std::os::fd::AsRawFd;
        use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};

        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(canonical)
            .map_err(|error| match error.raw_os_error() {
                Some(libc::ELOOP) => outside(),
                _ => error.into(),
            })?;
        let mut path = [0u8; libc::MAXPATHLEN as usize];
        // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes to the provided valid buffer.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) };
        if result == -1 {
            return Err(cannot_verify());
        }
        let len = path
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(cannot_verify)?;
        let opened = Path::new(std::ffi::OsStr::from_bytes(&path[..len]));
        if opened != canonical {
            return Err(outside());
        }
        Ok(file)
    }

    #[cfg(windows)]
    fn open_checked(&self, canonical: &Path) -> Result<File, AssetResolveError> {
        use std::ffi::OsString;
        use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_NAME_NORMALIZED, FILE_SHARE_READ, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
        };

        // Refuse concurrent write/delete access to this file while checking and reading it.
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(canonical)?;
        let mut path = vec![0u16; 512];
        let opened = loop {
            // SAFETY: the file owns a live handle and the path buffer is writable for its length.
            let len = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle(),
                    path.as_mut_ptr(),
                    path.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if len == 0 || len > 32_768 {
                return Err(cannot_verify());
            }
            if len < path.len() {
                break PathBuf::from(OsString::from_wide(&path[..len]));
            }
            path.resize(len + 1, 0);
        };
        // Keep the canonical verbatim prefix (including UNC), and compare path components,
        // not a case-folded string. Unverifiable aliases are rejected rather than guessed at.
        if opened != canonical {
            return Err(outside());
        }
        Ok(file)
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    fn open_checked(&self, _canonical: &Path) -> Result<File, AssetResolveError> {
        Err(AssetResolveError::UnsafePath(
            "filesystem containment checks are not supported on this platform",
        ))
    }
}

impl AssetResolver for FsAssetResolver {
    fn resolve(&self, path: &str) -> Result<Vec<u8>, AssetResolveError> {
        let relative = portable_relative_path(path)?;
        let canonical = fs::canonicalize(self.root.join(relative))?;
        if canonical == self.root || !canonical.starts_with(&self.root) {
            return Err(outside());
        }
        // This preliminary check avoids opening known non-regular files. It is not authoritative:
        // check that the actual opened file is regular and enforce the actual read length below.
        if !fs::metadata(&canonical)?.is_file() {
            return Err(not_a_file());
        }
        let mut file = self.open_checked(&canonical)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(not_a_file());
        }
        if meta.len() > MAX_ASSET_BYTES {
            return Err(AssetResolveError::TooLarge {
                limit: MAX_ASSET_BYTES,
            });
        }
        read_bounded(&mut file, MAX_ASSET_BYTES)
    }
}

fn outside() -> AssetResolveError {
    AssetResolveError::UnsafePath("resolved path leaves the workspace or changed during lookup")
}

#[cfg(any(windows, target_os = "macos"))]
fn cannot_verify() -> AssetResolveError {
    AssetResolveError::UnsafePath("cannot verify the opened file path")
}

fn not_a_file() -> AssetResolveError {
    AssetResolveError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected a regular file",
    ))
}

fn portable_relative_path(path: &str) -> Result<PathBuf, AssetResolveError> {
    // Check both path separators even on Unix. Colon excludes drive-relative prefixes, URLs,
    // and NTFS alternate streams; reject Windows device names and ambiguous Win32 trimming.
    if path.starts_with(['/', '\\']) || path.contains([':', '\0']) {
        return Err(AssetResolveError::UnsafePath(
            "expected a relative asset path",
        ));
    }
    let mut relative = PathBuf::new();
    for part in path
        .split(['/', '\\'])
        .filter(|part| !matches!(*part, "" | "."))
    {
        if part == ".." || part.ends_with(['.', ' ']) || is_windows_device(part) {
            return Err(AssetResolveError::UnsafePath("forbidden path component"));
        }
        relative.push(part);
    }
    if relative.as_os_str().is_empty() {
        return Err(AssetResolveError::UnsafePath("asset path is empty"));
    }
    Ok(relative)
}

fn is_windows_device(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn read_bounded(reader: &mut impl Read, limit: u64) -> Result<Vec<u8>, AssetResolveError> {
    let mut bytes = Vec::new();
    reader.by_ref().take(limit).read_to_end(&mut bytes)?;
    if bytes.len() as u64 == limit {
        // Probe separately, so the overflow byte cannot make the asset buffer grow.
        match reader.read_exact(&mut [0]) {
            Ok(()) => return Err(AssetResolveError::TooLarge { limit }),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(bytes)
}

/// Stable lowercase SHA-256 hex digest of the exact asset buffer passed to the importer.
pub fn asset_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

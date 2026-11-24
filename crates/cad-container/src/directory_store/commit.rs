use std::{
    borrow::Cow,
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::ContainerError;

pub(crate) static DIRECTORY_IO: RwLock<()> = RwLock::new(());

pub(crate) fn poisoned() -> ContainerError {
    io::Error::other("a directory storage operation panicked").into()
}

/// Prepare replacements and backups before updating any live project file.
pub(super) fn commit<'a>(
    root: &Path,
    entries: impl IntoIterator<Item = (PathBuf, Option<Cow<'a, [u8]>>)>,
) -> Result<(), ContainerError> {
    fs::create_dir_all(root)?;
    let mut stage = Stage::new(root)?;
    let changes = entries
        .into_iter()
        .enumerate()
        .map(|(index, (target, bytes))| {
            Change::prepare(root, &stage.path, index, target, bytes.as_deref())
        })
        .collect::<io::Result<Vec<_>>>()?;

    for (index, change) in changes.iter().enumerate() {
        if let Err(save_error) = change.apply() {
            let mut rollback_error = None;
            for applied in changes[..index].iter().rev() {
                if let Err(error) = applied.rollback() {
                    rollback_error.get_or_insert(error);
                }
            }
            if let Some(error) = rollback_error {
                stage.clean_up = false;
                return Err(io::Error::other(format!(
                    "directory save failed ({save_error}) and could not roll back ({error}); backups are in {}",
                    stage.path.display()
                ))
                .into());
            }
            return Err(save_error.into());
        }
    }
    Ok(())
}

struct Change {
    target: PathBuf,
    replacement: Option<PathBuf>,
    backup: Option<PathBuf>,
}

impl Change {
    fn prepare(
        root: &Path,
        stage: &Path,
        index: usize,
        target: PathBuf,
        bytes: Option<&[u8]>,
    ) -> io::Result<Self> {
        let replacement = if let Some(bytes) = bytes {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let replacement = stage.join(format!("{index}.new"));
            fs::write(&replacement, bytes)?;
            Some(replacement)
        } else {
            None
        };
        let backup = match fs::symlink_metadata(&target) {
            Ok(_) => {
                let relative = target.strip_prefix(root).map_err(io::Error::other)?;
                let backup = stage.join("backup").join(relative);
                if let Some(parent) = backup.parent() {
                    fs::create_dir_all(parent)?;
                }
                if fs::hard_link(&target, &backup).is_err() {
                    fs::copy(&target, &backup)?;
                }
                Some(backup)
            }
            Err(error) if error.kind() == ErrorKind::NotFound && replacement.is_some() => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            target,
            replacement,
            backup,
        })
    }

    fn apply(&self) -> io::Result<()> {
        match &self.replacement {
            Some(path) => fs::rename(path, &self.target),
            None => fs::remove_file(&self.target),
        }
    }

    fn rollback(&self) -> io::Result<()> {
        match &self.backup {
            Some(path) => fs::rename(path, &self.target),
            None => fs::remove_file(&self.target),
        }
    }
}

struct Stage {
    path: PathBuf,
    clean_up: bool,
}

impl Stage {
    fn new(root: &Path) -> io::Result<Self> {
        static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
        loop {
            let id = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!(".aicad-stage-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        clean_up: true,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.clean_up {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

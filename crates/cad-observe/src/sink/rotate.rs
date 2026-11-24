//! Size-rotating append-only log file. Never panics; degrades to dropping lines.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_KEEP: usize = 5;

pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

fn archive_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl RotatingFile {
    pub fn open(path: PathBuf, max_bytes: u64, keep: usize) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = open_append(&path)?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Self {
            path,
            max_bytes: max_bytes.max(1),
            keep,
            file: Some(file),
            size,
        })
    }

    /// Append one line (a trailing newline is added). Errors are swallowed.
    pub fn write_line(&mut self, line: &str) {
        let len = line.len() as u64 + 1;
        if self.size > 0 && self.size + len > self.max_bytes {
            self.rotate();
        }
        if self.file.is_none() {
            self.file = open_append(&self.path).ok();
            self.size = self
                .file
                .as_ref()
                .and_then(|f| f.metadata().ok())
                .map_or(0, |m| m.len());
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        if file.write_all(buf.as_bytes()).is_ok() {
            self.size += len;
        } else {
            self.file = None;
        }
    }

    fn rotate(&mut self) {
        // Windows cannot rename an open file: close the handle first.
        self.file = None;
        self.size = 0;
        if self.keep == 0 {
            let _ = fs::remove_file(&self.path);
        } else {
            let _ = fs::remove_file(archive_path(&self.path, self.keep));
            for index in (1..self.keep).rev() {
                let from = archive_path(&self.path, index);
                if from.exists() {
                    let _ = fs::rename(&from, archive_path(&self.path, index + 1));
                }
            }
            if fs::rename(&self.path, archive_path(&self.path, 1)).is_err() {
                // Rename blocked (e.g. another handle on Windows): truncate in place.
                let _ = File::create(&self.path);
            }
        }
        self.file = open_append(&self.path).ok();
    }
}

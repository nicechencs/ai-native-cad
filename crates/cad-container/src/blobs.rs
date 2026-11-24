//! Content-addressed storage for external graph payloads.

use cad_ir::{BlobHandle, BlobKind, BlobManifestEntry};
use sha2::{Digest, Sha256};

use crate::{AicadProject, ContainerError};

/// Return the SHA-256 digest as lower-case hexadecimal.
pub fn compute_sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Normalize a path used inside an `.aicad` container.
///
/// Container paths use `/` and have one spelling on supported platforms.
pub(crate) fn normalize_relative_path(path: &str) -> Result<String, ContainerError> {
    if path.split('/').any(|component| {
        component.is_empty()
            || component.ends_with(['.', ' '])
            || component.chars().any(|character| {
                character.is_control()
                    || matches!(character, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
            })
            || is_windows_device(component)
    }) {
        return Err(ContainerError::UnsafePath(path.to_owned()));
    }
    Ok(path.to_owned())
}

fn is_windows_device(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(
                number,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

pub(crate) fn checked_blob_path(path: &str) -> Result<String, ContainerError> {
    let path = normalize_relative_path(path)?;
    if !path.starts_with("blobs/") {
        return Err(ContainerError::UnsafePath(path));
    }
    Ok(path)
}

fn blob_relative_path(kind: BlobKind, digest: &str) -> String {
    let directory = match kind {
        BlobKind::Spline => "splines",
        BlobKind::SubD => "subd",
        BlobKind::Field => "fields",
        BlobKind::Import => "imports",
    };
    format!("blobs/{directory}/{digest}.bin")
}

impl AicadProject {
    /// Insert a blob and register its content-addressed graph manifest entry.
    pub fn put_blob(
        &mut self,
        handle: BlobHandle,
        kind: BlobKind,
        bytes: Vec<u8>,
    ) -> Result<BlobManifestEntry, ContainerError> {
        let digest = compute_sha256_hex(&bytes);
        let relative_path = blob_relative_path(kind, &digest);
        let previous_path = self
            .graph
            .blobs
            .get(&handle.0)
            .map(|previous| checked_blob_path(&previous.relative_path))
            .transpose()?;
        let entry = BlobManifestEntry {
            handle: handle.clone(),
            kind,
            sha256: digest,
            byte_len: bytes.len() as u64,
            relative_path,
        };
        self.blobs.insert(entry.relative_path.clone(), bytes);
        self.graph.blobs.insert(handle.0, entry.clone());
        if let Some(path) = previous_path
            && !self
                .graph
                .blobs
                .values()
                .any(|entry| entry.relative_path == path)
        {
            self.blobs.remove(&path);
        }
        Ok(entry)
    }

    /// Verify that every graph blob points at an existing, unmodified payload.
    pub fn verify_all_blobs(&self) -> Result<(), ContainerError> {
        for entry in self.graph.blobs.values() {
            let relative_path = checked_blob_path(&entry.relative_path)?;
            let Some(bytes) = self.blobs.get(&relative_path) else {
                return Err(ContainerError::MissingBlob(relative_path));
            };

            if bytes.len() as u64 != entry.byte_len || compute_sha256_hex(bytes) != entry.sha256 {
                return Err(ContainerError::BlobHashMismatch {
                    path: relative_path,
                });
            }
        }
        Ok(())
    }
}

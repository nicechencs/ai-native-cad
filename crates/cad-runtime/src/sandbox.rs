//! Workspace path sandbox.
//!
//! A document path must stay inside the caller-supplied workspace root.
//! `..` segments, control characters, NTFS alternate data streams (`:`), and
//! reserved Windows device names are rejected before any filesystem write.
//! Every path component is inspected with `symlink_metadata` so existing or
//! dangling symlinks cannot escape the workspace root.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::error::RuntimeError;

const MAX_SYMLINK_HOPS: usize = 40;

/// Resolve `user_path` inside `workspace_root`.
///
/// Relative paths join the workspace. Absolute paths must already land inside
/// it. The returned path uses the canonical spelling of the longest existing
/// prefix, with any not-yet-created tail appended. Any symlink (including a
/// dangling symlink) whose target leaves `workspace_root` is rejected.
pub fn resolve_in_workspace(workspace_root: &Path, user_path: &Path) -> Result<PathBuf, RuntimeError> {
    let root = canonicalize_workspace(workspace_root)?;
    if user_path.as_os_str().is_empty()
        || has_parent_dir(user_path)
        || has_forbidden_segment(user_path)
    {
        return Err(escape(user_path, &root));
    }

    let resolved = if user_path.is_absolute() {
        if let Some(relative) = strip_workspace_prefix(workspace_root, &root, user_path) {
            resolve_relative_from_root(&root, &relative, user_path)?
        } else {
            resolve_absolute_checked(&root, user_path)?
        }
    } else {
        if has_prefix_or_root(user_path) {
            return Err(escape(user_path, &root));
        }
        resolve_relative_from_root(&root, user_path, user_path)?
    };

    if !path_is_inside(&root, &resolved) {
        return Err(escape(user_path, &root));
    }
    Ok(resolved)
}

pub(crate) fn canonicalize_workspace(workspace_root: &Path) -> Result<PathBuf, RuntimeError> {
    let meta = std::fs::metadata(workspace_root)?;
    if !meta.is_dir() {
        return Err(RuntimeError::Invalid(format!(
            "workspace root `{}` is not a directory",
            workspace_root.display()
        )));
    }
    Ok(workspace_root.canonicalize()?)
}

fn escape(path: &Path, workspace_root: &Path) -> RuntimeError {
    RuntimeError::PathEscape {
        path: path.display().to_string(),
        workspace_root: workspace_root.display().to_string(),
    }
}

fn has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn has_prefix_or_root(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
}

fn has_forbidden_segment(path: &Path) -> bool {
    if path.to_string_lossy().chars().any(char::is_control) {
        return true;
    }
    for component in path.components() {
        if let Component::Normal(seg) = component {
            if is_forbidden_component(seg) {
                return true;
            }
        }
    }
    false
}

fn is_forbidden_component(seg: &OsStr) -> bool {
    let text = seg.to_string_lossy();
    if text.is_empty() || text.chars().any(char::is_control) || text.contains(':') {
        return true;
    }
    let trimmed = text.trim_end_matches(|ch: char| ch == '.' || ch == ' ');
    if trimmed.is_empty() {
        return true;
    }
    let stem = trimmed.split('.').next().unwrap_or("");
    let lower = stem.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    )
}

fn strip_workspace_prefix(workspace_root: &Path, canonical_root: &Path, user_path: &Path) -> Option<PathBuf> {
    if let Some(rel) = strip_normalized_prefix(user_path, canonical_root) {
        return Some(rel);
    }
    if workspace_root.is_absolute() {
        if let Some(rel) = strip_normalized_prefix(user_path, workspace_root) {
            return Some(rel);
        }
    } else if let Ok(abs_root) = std::path::absolute(workspace_root) {
        if let Some(rel) = strip_normalized_prefix(user_path, &abs_root) {
            return Some(rel);
        }
    }
    None
}

fn strip_normalized_prefix(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let norm_path = normalize_for_compare(path);
    let norm_prefix = normalize_for_compare(prefix);
    if !norm_path.starts_with(&norm_prefix) {
        return None;
    }
    let prefix_count = norm_prefix.components().count();
    let mut rel = PathBuf::new();
    for component in path.components().skip(prefix_count) {
        rel.push(component.as_os_str());
    }
    Some(rel)
}

fn resolve_relative_from_root(
    root: &Path,
    relative: &Path,
    user_path: &Path,
) -> Result<PathBuf, RuntimeError> {
    let mut hops = 0usize;
    let queue = collect_components(relative);
    resolve_walk(root.to_path_buf(), queue, root, user_path, true, &mut hops)
}

fn resolve_absolute_checked(root: &Path, user_path: &Path) -> Result<PathBuf, RuntimeError> {
    let mut hops = 0usize;
    let queue = collect_components(user_path);
    resolve_walk(PathBuf::new(), queue, root, user_path, false, &mut hops)
}

#[derive(Debug, Clone)]
enum Step {
    Prefix(OsString),
    RootDir,
    ParentDir,
    Normal(OsString),
}

fn collect_components(path: &Path) -> VecDeque<Step> {
    let mut out = VecDeque::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push_back(Step::Prefix(prefix.as_os_str().to_os_string())),
            Component::RootDir => out.push_back(Step::RootDir),
            Component::CurDir => {}
            Component::ParentDir => out.push_back(Step::ParentDir),
            Component::Normal(name) => out.push_back(Step::Normal(name.to_os_string())),
        }
    }
    out
}

fn resolve_walk(
    mut current: PathBuf,
    mut queue: VecDeque<Step>,
    root: &Path,
    user_path: &Path,
    enforce_inside_root: bool,
    hops: &mut usize,
) -> Result<PathBuf, RuntimeError> {
    let mut missing = false;
    let mut inside_root = enforce_inside_root || path_is_inside_or_eq(root, &current);

    while let Some(step) = queue.pop_front() {
        match step {
            Step::Prefix(prefix) => {
                current.push(prefix);
            }
            Step::RootDir => {
                current.push(Component::RootDir.as_os_str());
            }
            Step::ParentDir => {
                if !current.pop() {
                    return Err(escape(user_path, root));
                }
                if inside_root && !path_is_inside_or_eq(root, &current) {
                    return Err(escape(user_path, root));
                }
                missing = !path_exists_exact(&current);
            }
            Step::Normal(name) => {
                if inside_root && is_forbidden_component(&name) {
                    return Err(escape(user_path, root));
                }
                if missing {
                    current.push(&name);
                    continue;
                }
                let candidate = current.join(&name);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(meta) => {
                        if let Some(link_target) = read_link_target(&candidate, &meta)? {
                            *hops += 1;
                            if *hops > MAX_SYMLINK_HOPS {
                                return Err(escape(user_path, root));
                            }
                            let target_steps = collect_components(&link_target);
                            let target_base = if link_target.is_absolute()
                                || has_prefix_or_root(&link_target)
                            {
                                PathBuf::new()
                            } else {
                                current.clone()
                            };
                            let resolved_target = resolve_walk(
                                target_base,
                                target_steps,
                                root,
                                user_path,
                                false,
                                hops,
                            )?;
                            if inside_root && !path_is_inside_or_eq(root, &resolved_target) {
                                return Err(escape(user_path, root));
                            }
                            if inside_root && has_forbidden_relative_segments(root, &resolved_target) {
                                return Err(escape(user_path, root));
                            }
                            current = resolved_target;
                            inside_root =
                                enforce_inside_root || path_is_inside_or_eq(root, &current);
                            missing = !path_exists_exact(&current);
                        } else {
                            current = candidate;
                            if !inside_root && path_is_inside_or_eq(root, &current) {
                                inside_root = true;
                            }
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::NotFound => {
                        if !current.as_os_str().is_empty() {
                            current = canonicalize_existing_prefix(&current, root, user_path)?;
                        }
                        if inside_root && !path_is_inside_or_eq(root, &current) {
                            return Err(escape(user_path, root));
                        }
                        current.push(&name);
                        missing = true;
                    }
                    Err(err) => return Err(RuntimeError::Io(err)),
                }
            }
        }
    }

    if !missing && !current.as_os_str().is_empty() {
        current = canonicalize_existing_prefix(&current, root, user_path)?;
    }
    Ok(current)
}

fn canonicalize_existing_prefix(
    path: &Path,
    root: &Path,
    user_path: &Path,
) -> Result<PathBuf, RuntimeError> {
    match path.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(err) if err.kind() == ErrorKind::PermissionDenied => Err(escape(user_path, root)),
        Err(err) => Err(RuntimeError::Io(err)),
    }
}

fn path_exists_exact(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn read_link_target(
    candidate: &Path,
    meta: &std::fs::Metadata,
) -> Result<Option<PathBuf>, RuntimeError> {
    if meta.file_type().is_symlink() {
        return Ok(Some(std::fs::read_link(candidate)?));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            if let Ok(target) = std::fs::read_link(candidate) {
                return Ok(Some(target));
            }
        }
    }
    Ok(None)
}

fn has_forbidden_relative_segments(root: &Path, path: &Path) -> bool {
    if let Some(rel) = strip_normalized_prefix(path, root) {
        has_forbidden_segment(&rel)
    } else {
        false
    }
}

fn path_is_inside(root: &Path, path: &Path) -> bool {
    let root = normalize_for_compare(root);
    let path = normalize_for_compare(path);
    path.starts_with(&root) && path != root
}

fn path_is_inside_or_eq(root: &Path, path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    let root = normalize_for_compare(root);
    let path = normalize_for_compare(path);
    path.starts_with(&root)
}

fn normalize_for_compare(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let stripped = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| text.into_owned());
    #[cfg(windows)]
    {
        PathBuf::from(stripped.replace('/', "\\").to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(stripped)
    }
}

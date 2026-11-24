//! Tauri IPC for reading `.cadpack` sessions and seeding the built-in demo.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use cad_cadpack::{
    AssemblyTreeNode, CadpackReader, CadpackWriter, ChunkId, ChunkKind, HlodLevel,
    PrototypeHlodChunk, VirtualAssemblyIndex, build_roi_interface_digest,
};
use serde::{Deserialize, Serialize};
use tauri::{State, ipc::Response};

use crate::{DesktopWorkspace, project_commands::resolve_project_path};

const DEFAULT_SEARCH_LIMIT: usize = 100;
const MAX_HLOD_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

pub type VirtualTreeNode = AssemblyTreeNode;

#[derive(Debug, PartialEq, Serialize)]
pub struct TreePageSlice {
    pub offset: usize,
    pub limit: usize,
    pub total_count: usize,
    pub nodes: Vec<VirtualTreeNode>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct RoiInterfaceDigest {
    pub roi_min_mm: [f64; 3],
    pub roi_max_mm: [f64; 3],
    pub component_count: usize,
    pub summary_json: String,
}

#[derive(Debug, Deserialize)]
pub struct FetchTreeSliceRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub session_id: String,
    pub offset: usize,
    pub limit: usize,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchAssemblyTreeRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub session_id: String,
    pub query: String,
    pub limit: Option<usize>,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FetchHlodChunkRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub session_id: String,
    pub chunk_id: ChunkId,
    pub byte_range: Option<[u64; 2]>,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FetchInterfaceDigestRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub session_id: String,
    pub roi_min_mm: [f64; 3],
    pub roi_max_mm: [f64; 3],
    pub workspace_root: Option<String>,
}

#[tauri::command]
pub async fn fetch_tree_slice(
    request: FetchTreeSliceRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<TreePageSlice, String> {
    let trace_id = request.trace_id.clone();
    in_workspace(&workspace, request.workspace_root.clone(), move |root| {
        crate::observe::traced("fetch_tree_slice", trace_id.as_deref(), |_| {
            fetch_tree_slice_in(root, &request.session_id, request.offset, request.limit)
        })
    })
    .await
}

// The TypeScript transport parses the IPC search result as instance IDs.
#[tauri::command]
pub async fn search_assembly_tree(
    request: SearchAssemblyTreeRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<Vec<u32>, String> {
    let trace_id = request.trace_id.clone();
    in_workspace(&workspace, request.workspace_root.clone(), move |root| {
        crate::observe::traced("search_assembly_tree", trace_id.as_deref(), |_| {
            let page =
                search_assembly_tree_in(root, &request.session_id, &request.query, request.limit)?;
            Ok(page
                .nodes
                .into_iter()
                .map(|node| node.instance_id)
                .collect())
        })
    })
    .await
}

#[tauri::command]
pub async fn fetch_hlod_chunk(
    request: FetchHlodChunkRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<Response, String> {
    let trace_id = request.trace_id.clone();
    in_workspace(&workspace, request.workspace_root.clone(), move |root| {
        crate::observe::traced("fetch_hlod_chunk", trace_id.as_deref(), |_| {
            fetch_hlod_chunk_in(
                root,
                &request.session_id,
                request.chunk_id,
                request.byte_range,
            )
        })
    })
    .await
    .map(Response::new)
}

#[tauri::command]
pub async fn fetch_interface_digest(
    request: FetchInterfaceDigestRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<RoiInterfaceDigest, String> {
    in_workspace(&workspace, request.workspace_root, move |root| {
        fetch_interface_digest_in(
            root,
            &request.session_id,
            request.roi_min_mm,
            request.roi_max_mm,
        )
    })
    .await
}

async fn in_workspace<T: Send + 'static>(
    workspace: &DesktopWorkspace,
    requested_root: Option<String>,
    read: impl FnOnce(&Path) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let workspace = workspace.clone();
    tauri::async_runtime::spawn_blocking(move || {
        read(workspace.root_for(requested_root.as_deref())?)
    })
    .await
    .map_err(|err| err.to_string())?
}

pub fn fetch_tree_slice_in(
    root: &Path,
    session_id: &str,
    offset: usize,
    limit: usize,
) -> Result<TreePageSlice, String> {
    let tree = load_tree(root, session_id)?;
    Ok(TreePageSlice {
        offset,
        limit,
        total_count: tree.len(),
        nodes: tree.into_iter().skip(offset).take(limit).collect(),
    })
}

pub fn search_assembly_tree_in(
    root: &Path,
    session_id: &str,
    query: &str,
    limit: Option<usize>,
) -> Result<TreePageSlice, String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Err("Assembly search query must be non-empty".into());
    }
    let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    let mut page = TreePageSlice {
        offset: 0,
        limit,
        total_count: 0,
        nodes: Vec::new(),
    };
    for node in load_tree(root, session_id)? {
        if node.name.to_lowercase().contains(&query) {
            page.total_count += 1;
            if page.nodes.len() < limit {
                page.nodes.push(node);
            }
        }
    }
    Ok(page)
}

pub fn fetch_hlod_chunk_in(
    root: &Path,
    session_id: &str,
    chunk_id: ChunkId,
    byte_range: Option<[u64; 2]>,
) -> Result<Vec<u8>, String> {
    let range = byte_range.map(hlod_range).transpose()?;
    let mut file = open_session(root, session_id)?;
    let mut reader = CadpackReader::new(&mut file).map_err(|err| err.to_string())?;
    let entry = reader
        .directory()
        .iter()
        .find(|entry| entry.chunk_id == chunk_id)
        .copied()
        .ok_or_else(|| format!("Chunk {chunk_id} was not found"))?;
    if !matches!(
        entry.kind,
        ChunkKind::Hlod0 | ChunkKind::Hlod1 | ChunkKind::Hlod2
    ) {
        return Err(format!("Chunk {chunk_id} is not an HLOD mesh"));
    }
    let (start, length) = range.unwrap_or((0, entry.length));
    if length > MAX_HLOD_CHUNK_BYTES {
        return Err("HLOD chunk reads must be at most 16 MiB".into());
    }
    if length > entry.length.saturating_sub(start) {
        return Err("HLOD byte range lies outside the chunk".into());
    }
    if start == 0 && length == entry.length {
        return reader
            .read_chunk_bytes(chunk_id)
            .map_err(|err| err.to_string());
    }

    // The parsed directory bounds the chunk, so this seek cannot leave its payload.
    let mut bytes = vec![0; length as usize];
    file.seek(SeekFrom::Start(entry.offset + start))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|err| format!("Cannot read HLOD byte range: {err}"))?;
    Ok(bytes)
}

fn hlod_range([start, end]: [u64; 2]) -> Result<(u64, u64), String> {
    let span = end
        .checked_sub(start)
        .ok_or("HLOD byte range is reversed")?;
    if span >= MAX_HLOD_CHUNK_BYTES {
        return Err("HLOD byte range must be at most 16 MiB".into());
    }
    Ok((start, span + 1))
}

pub fn fetch_interface_digest_in(
    root: &Path,
    session_id: &str,
    roi_min_mm: [f64; 3],
    roi_max_mm: [f64; 3],
) -> Result<RoiInterfaceDigest, String> {
    if roi_min_mm
        .into_iter()
        .chain(roi_max_mm)
        .any(|value| !value.is_finite())
    {
        return Err("ROI coordinates must be finite".into());
    }
    let tree = VirtualAssemblyIndex::new(load_tree(root, session_id)?);
    let digest = build_roi_interface_digest(&tree, roi_min_mm, roi_max_mm);
    Ok(RoiInterfaceDigest {
        roi_min_mm,
        roi_max_mm,
        component_count: digest.total_matches,
        summary_json: digest.to_json().map_err(|err| err.to_string())?,
    })
}

#[derive(Deserialize)]
struct VirtualTreePage {
    offset: usize,
    total: usize,
    nodes: Vec<VirtualTreeNode>,
}

fn decode_tree_page(bytes: &[u8]) -> Result<VirtualTreePage, serde_json::Error> {
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'[') {
        let nodes: Vec<_> = serde_json::from_slice(bytes)?;
        return Ok(VirtualTreePage {
            offset: 0,
            total: nodes.len(),
            nodes,
        });
    }
    serde_json::from_slice(bytes)
}

fn load_tree(root: &Path, session_id: &str) -> Result<Vec<VirtualTreeNode>, String> {
    let mut reader =
        CadpackReader::new(open_session(root, session_id)?).map_err(|err| err.to_string())?;
    let chunk_ids: Vec<_> = reader
        .directory()
        .iter()
        .filter(|entry| entry.kind == ChunkKind::VirtualTree)
        .map(|entry| entry.chunk_id)
        .collect();
    if chunk_ids.is_empty() {
        return Err("Session has no virtual assembly tree".into());
    }
    let mut pages = Vec::new();
    for chunk_id in chunk_ids {
        let bytes = reader
            .read_chunk_bytes(chunk_id)
            .map_err(|err| err.to_string())?;
        let page = decode_tree_page(&bytes)
            .map_err(|err| format!("Cannot decode virtual tree chunk {chunk_id}: {err}"))?;
        pages.push(page);
    }
    pages.sort_by_key(|page| page.offset);
    let total_count = pages[0].total;
    let mut tree = Vec::new();
    for VirtualTreePage {
        offset,
        total,
        nodes,
    } in pages
    {
        if offset != tree.len()
            || total != total_count
            || nodes.len() > total.saturating_sub(offset)
        {
            return Err("Virtual assembly tree pages are inconsistent".into());
        }
        tree.extend(nodes);
    }
    if tree.len() != total_count {
        return Err("Virtual assembly tree pages are incomplete".into());
    }
    Ok(tree)
}

fn open_session(root: &Path, session_id: &str) -> Result<File, String> {
    if session_id.is_empty()
        || session_id == "."
        || session_id.contains("..")
        || session_id.contains(['/', '\\', ':', '?', '#'])
        || session_id.chars().any(char::is_control)
    {
        return Err("Session ID must be a single filename inside the desktop workspace".into());
    }
    let path = resolve_project_path(root, &format!("{session_id}.cadpack"))?;
    let metadata = match path.symlink_metadata() {
        Err(err) if session_id == "demo" && err.kind() == ErrorKind::NotFound => {
            seed_demo_session(&path).map_err(|err| format!("Cannot seed demo CAD pack: {err}"))?;
            path.symlink_metadata()
        }
        result => result,
    }
    .map_err(|err| format!("Cannot access session CAD pack: {err}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Session CAD pack must be a regular file inside the desktop workspace".into());
    }
    File::open(path).map_err(|err| format!("Cannot open session CAD pack: {err}"))
}

fn seed_demo_session(path: &Path) -> io::Result<()> {
    let tree = VirtualAssemblyIndex::new(vec![
        AssemblyTreeNode {
            instance_id: 1,
            parent_id: None,
            prototype_id: 0,
            name: "Stepper drive assembly".into(),
            part_number: "DRV-ASM-001".into(),
            bbox_min_mm: [0.0, 0.0, 0.0],
            bbox_max_mm: [140.0, 90.0, 78.0],
            frozen_lightweight: true,
        },
        AssemblyTreeNode {
            instance_id: 2,
            parent_id: Some(1),
            prototype_id: 1,
            name: "Aluminum mounting plate".into(),
            part_number: "DRV-PLT-140".into(),
            bbox_min_mm: [0.0, 0.0, 0.0],
            bbox_max_mm: [140.0, 90.0, 12.0],
            frozen_lightweight: true,
        },
        AssemblyTreeNode {
            instance_id: 3,
            parent_id: Some(1),
            prototype_id: 2,
            name: "NEMA 23 stepper motor".into(),
            part_number: "DRV-MTR-023".into(),
            bbox_min_mm: [42.0, 16.0, 12.0],
            bbox_max_mm: [98.0, 72.0, 70.0],
            frozen_lightweight: true,
        },
        AssemblyTreeNode {
            instance_id: 4,
            parent_id: Some(1),
            prototype_id: 3,
            name: "Terminal cover".into(),
            part_number: "DRV-CVR-080".into(),
            bbox_min_mm: [30.0, 10.0, 70.0],
            bbox_max_mm: [110.0, 80.0, 78.0],
            frozen_lightweight: true,
        },
    ]);

    let mut writer = CadpackWriter::new();
    let tree_bytes = serde_json::to_vec(&tree.page_slice(0, tree.len()))?;
    writer
        .add_chunk(40, ChunkKind::VirtualTree, tree_bytes)
        .map_err(io::Error::other)?;
    for node in tree.nodes() {
        for (level, kind) in [
            (HlodLevel::Hlod0Fine, ChunkKind::Hlod0),
            (HlodLevel::Hlod1Medium, ChunkKind::Hlod1),
            (HlodLevel::Hlod2Coarse, ChunkKind::Hlod2),
        ] {
            let chunk_id = node.prototype_id * 10 + u32::from(level.as_u8());
            let bytes = demo_hlod_box(node, level)
                .encode()
                .map_err(io::Error::other)?;
            writer
                .add_chunk(chunk_id, kind, bytes)
                .map_err(io::Error::other)?;
        }
    }

    // Publish the complete pack without replacing one created by another request.
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let (stage, file) = loop {
        let stage = path.with_file_name(format!(
            ".demo-cadpack-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed),
        ));
        match File::create_new(&stage) {
            Ok(file) => break (stage, file),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    };
    let result = writer
        .write_to(file)
        .map_err(io::Error::other)
        .and_then(|()| fs::hard_link(&stage, path));
    let _ = fs::remove_file(&stage);
    match result {
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

fn demo_hlod_box(node: &AssemblyTreeNode, level: HlodLevel) -> PrototypeHlodChunk {
    let anchor_mm = node.bbox_min_mm;
    let [x, y, z] = std::array::from_fn(|axis| node.bbox_max_mm[axis] - anchor_mm[axis]);
    // Separate vertices keep the winding, normals, and face slots distinct at each face.
    let faces = [
        ([0.0; 3], [0.0, 0.0, z], [0.0, y, 0.0], [-1.0, 0.0, 0.0]),
        ([x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, z], [1.0, 0.0, 0.0]),
        ([0.0; 3], [x, 0.0, 0.0], [0.0, 0.0, z], [0.0, -1.0, 0.0]),
        ([0.0, y, 0.0], [0.0, 0.0, z], [x, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0; 3], [0.0, y, 0.0], [x, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, 0.0, z], [x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, 1.0]),
    ];
    let divisions = 3 - u32::from(level.as_u8());
    let stride = divisions + 1;
    let mut chunk = PrototypeHlodChunk {
        prototype_id: node.prototype_id,
        level,
        anchor_mm,
        vertices_local: Vec::new(),
        normals: Vec::new(),
        face_slot_ids: Vec::new(),
        indices: Vec::new(),
    };
    for (slot, (origin, right, up, normal)) in (0..).zip(faces) {
        let base = slot * stride * stride;
        for row in 0..=divisions {
            for col in 0..=divisions {
                chunk.vertices_local.push(std::array::from_fn(|axis| {
                    (origin[axis]
                        + right[axis] * f64::from(col) / f64::from(divisions)
                        + up[axis] * f64::from(row) / f64::from(divisions))
                        as f32
                }));
                chunk.normals.push(normal);
                chunk.face_slot_ids.push(slot);
            }
        }
        for row in 0..divisions {
            for col in 0..divisions {
                let corner = base + row * stride + col;
                chunk.indices.extend([
                    corner,
                    corner + 1,
                    corner + stride + 1,
                    corner,
                    corner + stride + 1,
                    corner + stride,
                ]);
            }
        }
    }
    chunk
}

//! Cadpack export and queries, plus mesh-based manufacturing checks.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use cad_cadpack::{
    AssemblyTreeNode, CadpackReader, CadpackWriter, ChunkKind, HlodLevel, PrototypeHlodChunk,
    VirtualAssemblyIndex, build_roi_interface_digest,
};
use cad_evaluator::{GraphEvalReport, GraphEvaluator};
use cad_gpu_compute::{
    DfmConfig, DfmViolationKind, TriRef, check_assembly_interference, evaluate_raybeam_dfm,
};
use cad_ir::{CanonicalGraph, IrNode, IrType, IrValue, NodeId, NodeKind, NodeState};
use cad_kernel_occt::{BRepFace, GeometryKernel, Mat4};
use serde::Deserialize;
use serde_json::json;

use crate::project::{CommandOutput, ProjectFormat, input_vec3};

type Point = [f64; 3];

pub(super) fn parse_vec3(value: &str) -> Result<Point, String> {
    let coordinates = value
        .split(',')
        .map(|coordinate| coordinate.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "expected three finite coordinates separated by commas")?;
    match coordinates.as_slice() {
        &[x, y, z] if x.is_finite() && y.is_finite() && z.is_finite() => Ok([x, y, z]),
        _ => Err("expected three finite coordinates separated by commas".into()),
    }
}

pub(super) fn export(
    path: &Path,
    out: &Path,
    kernel: &dyn GeometryKernel,
) -> Result<CommandOutput> {
    let (bodies, _cache_hits) = evaluate_bodies(path, kernel)?;
    ensure!(
        out.canonicalize().ok().as_ref() != Some(&path.canonicalize()?),
        "cadpack output must differ from the input project"
    );

    let mut writer = CadpackWriter::new();
    let mut nodes = Vec::with_capacity(bodies.len());
    let mut bounds = Bounds::empty();
    for (index, body) in bodies.iter().enumerate() {
        let id = u32::try_from(index).context("too many cadpack instances")?;
        let body_bounds = body.mesh.bounds();
        bounds.include(body_bounds.min);
        bounds.include(body_bounds.max);
        nodes.push(body.tree_node(id, body_bounds));
        for (level, kind, divisions) in [
            (HlodLevel::Hlod0Fine, ChunkKind::Hlod0, None),
            (HlodLevel::Hlod1Medium, ChunkKind::Hlod1, Some(16)),
            (HlodLevel::Hlod2Coarse, ChunkKind::Hlod2, Some(4)),
        ] {
            let mesh = body.mesh.hlod(id, level, body_bounds, divisions)?;
            add_chunk(&mut writer, kind, mesh.encode()?)?;
        }
    }
    let index = VirtualAssemblyIndex::new(nodes);
    let digest = build_roi_interface_digest(&index, bounds.min, bounds.max);
    let digest_bytes = digest.to_json_bytes()?;
    let digest_size = digest_bytes.len();
    add_chunk(
        &mut writer,
        ChunkKind::VirtualTree,
        serde_json::to_vec(&index.page_slice(0, index.len()))?,
    )?;
    add_chunk(&mut writer, ChunkKind::InterfaceDigest, digest_bytes)?;
    write_pack(&writer, out)
        .with_context(|| format!("cannot write cadpack `{}`", out.display()))?;

    Ok(CommandOutput::report(
        format!(
            "Wrote cadpack with {} instances to {}",
            index.len(),
            out.display()
        ),
        json!({
            "status": "ok",
            "out": out,
            "instance_count": index.len(),
            "prototype_count": bodies.len(),
            "chunk_count": writer.chunk_count(),
            "digest_bytes": digest_size,
        }),
    ))
}

fn add_chunk(writer: &mut CadpackWriter, kind: ChunkKind, data: Vec<u8>) -> Result<()> {
    let id = u32::try_from(writer.chunk_count()).context("too many cadpack chunks")?;
    writer.add_chunk(id, kind, data)?;
    Ok(())
}

fn write_pack(writer: &CadpackWriter, out: &Path) -> Result<()> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary = out.with_file_name(format!(".cadpack-{}-{nonce}.tmp", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = writer
        .write_to(file)
        .map_err(anyhow::Error::from)
        .and_then(|()| Ok(fs::rename(&temporary, out)?));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Deserialize)]
struct VirtualTreePage {
    offset: usize,
    total: usize,
    nodes: Vec<AssemblyTreeNode>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VirtualTreeChunk {
    Page(VirtualTreePage),
    Nodes(Vec<AssemblyTreeNode>),
}

fn read_tree(path: &Path) -> Result<VirtualAssemblyIndex> {
    let file =
        File::open(path).with_context(|| format!("cannot open cadpack `{}`", path.display()))?;
    let mut reader = CadpackReader::new(file)
        .with_context(|| format!("cannot read cadpack `{}`", path.display()))?;
    let chunk_ids: Vec<_> = reader
        .directory()
        .iter()
        .filter(|entry| entry.kind == ChunkKind::VirtualTree)
        .map(|entry| entry.chunk_id)
        .collect();
    let mut pages = Vec::with_capacity(chunk_ids.len());
    for chunk_id in chunk_ids {
        let bytes = reader.read_chunk_bytes(chunk_id)?;
        let chunk: VirtualTreeChunk = serde_json::from_slice(&bytes)
            .with_context(|| format!("cannot decode virtual assembly tree chunk {chunk_id}"))?;
        pages.push(match chunk {
            VirtualTreeChunk::Page(page) => page,
            VirtualTreeChunk::Nodes(nodes) => VirtualTreePage {
                offset: 0,
                total: nodes.len(),
                nodes,
            },
        });
    }
    pages.sort_by_key(|page| page.offset);
    let total = pages
        .first()
        .context("cadpack has no virtual assembly tree")?
        .total;
    let mut nodes = Vec::new();
    for page in pages {
        ensure!(
            page.offset == nodes.len()
                && page.total == total
                && page.nodes.len() <= total.saturating_sub(page.offset),
            "cadpack virtual assembly tree pages are inconsistent"
        );
        nodes.extend(page.nodes);
    }
    ensure!(
        nodes.len() == total,
        "cadpack virtual assembly tree is incomplete"
    );
    Ok(VirtualAssemblyIndex::new(nodes))
}

pub(super) fn tree(
    path: &Path,
    offset: usize,
    limit: usize,
    query: Option<&str>,
) -> Result<CommandOutput> {
    let index = read_tree(path)?;
    if let Some(query) = query {
        let instance_ids = index.search_by_name(query, offset.saturating_add(limit));
        let instance_ids = &instance_ids[offset.min(instance_ids.len())..];
        return Ok(CommandOutput::report(
            format!(
                "{} matches for {query:?} from offset {offset}: {instance_ids:?}",
                instance_ids.len()
            ),
            json!({ "query": query, "offset": offset, "instance_ids": instance_ids }),
        ));
    }

    let page = index.page_slice(offset, limit);
    let mut lines = vec![format!(
        "{} of {} instances from offset {}",
        page.nodes.len(),
        page.total,
        page.offset,
    )];
    lines.extend(page.nodes.iter().map(|node| {
        format!(
            "{}  {}  ({})",
            node.instance_id, node.name, node.part_number
        )
    }));
    Ok(CommandOutput::report(
        lines.join("\n"),
        serde_json::to_value(page)?,
    ))
}

pub(super) fn digest(path: &Path, min: Point, max: Point) -> Result<CommandOutput> {
    ensure!(
        min.into_iter().zip(max).all(|(min, max)| min <= max),
        "--min must be less than or equal to --max on every axis"
    );
    let index = read_tree(path)?;
    let digest = build_roi_interface_digest(&index, min, max);
    let text = format!(
        "ROI intersects {} instances ({} summarized)",
        digest.total_matches,
        digest.components.len()
    );
    Ok(CommandOutput::report(text, serde_json::to_value(digest)?))
}

pub(super) fn dfm_check(
    path: &Path,
    min_wall_mm: f64,
    min_draft_deg: f64,
    pull_dir: Point,
    kernel: &dyn GeometryKernel,
) -> Result<CommandOutput> {
    ensure!(
        min_wall_mm.is_finite() && min_wall_mm >= 0.0,
        "--min-wall-mm must be finite and nonnegative"
    );
    ensure!(min_draft_deg.is_finite(), "--min-draft-deg must be finite");
    let (bodies, kept_node_cache_hits) = evaluate_bodies(path, kernel)?;
    let mut triangles = Vec::new();
    let mut face_instances = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        let instance_id = u32::try_from(index).context("too many DFM instances")?;
        let base = u32::try_from(face_instances.len()).context("too many DFM face slots")?;
        for &corners in &body.mesh.triangles {
            let [a, b, c] = corners.map(|index| body.mesh.vertices[index as usize]);
            triangles.push(TriRef {
                instance_id,
                face_slot_id: base
                    .checked_add(a.face_slot)
                    .context("too many DFM face slots")?,
                a: a.position,
                b: b.position,
                c: c.position,
            });
        }
        face_instances.extend((0..body.mesh.face_count).map(|slot| (instance_id, slot)));
    }

    let interference = check_assembly_interference(&triangles, 0.0);
    let direction_scale = pull_dir.into_iter().map(f64::abs).fold(0.0, f64::max);
    let dfm = evaluate_raybeam_dfm(
        &triangles,
        &DfmConfig {
            min_wall_thickness_mm: min_wall_mm,
            min_draft_angle_deg: min_draft_deg,
            pull_dir: if direction_scale > 0.0 {
                pull_dir.map(|coordinate| coordinate / direction_scale)
            } else {
                pull_dir
            },
            tool_axes: Vec::new(),
            samples_per_face: 16,
        },
    );
    let pairs: Vec<_> = interference
        .pairs
        .iter()
        .map(|pair| {
            json!({
                "instance_a": pair.instance_a,
                "instance_b": pair.instance_b,
                "face_a": face_instances[pair.face_a as usize].1,
                "face_b": face_instances[pair.face_b as usize].1,
                "min_distance_mm": pair.min_distance_mm,
                "penetrating": pair.penetrating,
            })
        })
        .collect();
    let faces: Vec<_> = dfm
        .faces
        .iter()
        .map(|face| {
            let (instance_id, face_slot_id) = face_instances[face.face_slot_id as usize];
            let violations: Vec<_> = face
                .violations
                .iter()
                .map(|kind| match kind {
                    DfmViolationKind::ThinWall => "thin_wall",
                    DfmViolationKind::DraftAngle => "draft_angle",
                    DfmViolationKind::ToolAccessibility => "tool_accessibility",
                })
                .collect();
            json!({
                "instance_id": instance_id,
                "face_slot_id": face_slot_id,
                "min_thickness_mm": face.min_thickness_mm,
                "draft_angle_deg": face.draft_angle_deg,
                "accessible": face.accessible,
                "violations": violations,
            })
        })
        .collect();

    Ok(CommandOutput::report(
        format!(
            "DFM checked {} instances and {} triangles. {} interference pairs, {} faces with violations.",
            bodies.len(),
            triangles.len(),
            pairs.len(),
            dfm.violation_count()
        ),
        json!({
            "status": "ok",
            "passed": interference.is_clear() && dfm.is_clear(),
            "instance_count": bodies.len(),
            "triangle_count": triangles.len(),
            "kept_node_cache_hits": kept_node_cache_hits,
            "instances": bodies.iter().enumerate().map(|(index, body)| {
                json!({ "instance_id": index, "name": body.name, "part_number": body.part_number })
            }).collect::<Vec<_>>(),
            "config": { "min_wall_mm": min_wall_mm, "min_draft_deg": min_draft_deg, "pull_dir": pull_dir },
            "interference": { "pair_count": pairs.len(), "pairs": pairs },
            "dfm": { "violation_count": dfm.violation_count(), "faces": faces },
        }),
    ))
}

fn evaluate_bodies(path: &Path, kernel: &dyn GeometryKernel) -> Result<(Vec<Body>, usize)> {
    let mut project = ProjectFormat::detect(path)?.load(path)?;
    let graph = &mut project.graph;
    let instances: Vec<_> = graph
        .nodes
        .values()
        .filter_map(|node| {
            if node.kind == NodeKind::AssemblyComponent
                && node.state != NodeState::Suppressed
                && let Some(IrValue::NodeRef { node_id, .. }) = node.inputs.get("geometry")
            {
                Some((node.id.clone(), node_id.clone()))
            } else {
                None
            }
        })
        .collect();
    let instanced_geometry: BTreeSet<_> = instances.iter().map(|(_, id)| id.clone()).collect();
    let mut seen = BTreeSet::new();
    let outputs: Vec<_> = graph
        .outputs
        .iter()
        .filter(|id| !instanced_geometry.contains(*id) && seen.insert((*id).clone()))
        .cloned()
        .collect();
    // GraphEvaluator only publishes meshes for declared outputs. Include
    // component geometry for this evaluation without modifying the saved graph.
    let original_output_len = graph.outputs.len();
    let mut requested: BTreeSet<_> = graph.outputs.iter().cloned().collect();
    graph.outputs.extend(
        instanced_geometry
            .into_iter()
            .filter(|id| requested.insert(id.clone())),
    );
    let mut evaluator = GraphEvaluator::new(kernel);
    let report = if graph.outputs.len() == original_output_len && storage_is_sqlite(path) {
        evaluator
            .evaluate_project_at_path(path, None)
            .context("cannot evaluate project meshes")?
    } else {
        evaluator.set_external_ref_roots(Some(path.to_path_buf()), None);
        evaluator
            .evaluate_graph_with_blobs(graph, &project.blobs, kernel)
            .context("cannot evaluate project meshes")?
    };

    let mut bodies = Vec::new();
    for id in outputs {
        let node = &graph.nodes[&id.0];
        if node.state == NodeState::Suppressed {
            continue;
        }
        if is_mesh_type(node.output_type) {
            bodies.extend(mesh_bodies(node, &report)?);
        } else if node.kind == NodeKind::AssemblyMate {
            require_valid(&id, &report)?;
        }
    }
    for (id, geometry) in instances {
        require_valid(&id, &report)?;
        let component = &graph.nodes[&id.0];
        let transform = component_transform(graph, component, &report)?;
        for mut body in mesh_bodies(&graph.nodes[&geometry.0], &report)? {
            if body.mesh.vertices.iter_mut().any(|vertex| {
                vertex.position = transform.transform_point(vertex.position);
                vertex.normal = transform.transform_dir(vertex.normal);
                !finite_point(vertex.position)
            }) {
                bail!("component `{id}` has non-finite transformed mesh coordinates");
            }
            body.name = if body.name == graph.nodes[&geometry.0].name {
                component.name.clone()
            } else {
                format!("{}/{}", component.name, body.name)
            };
            bodies.push(body);
        }
    }
    for (id, solid) in &report.component_solids {
        let Some(component) = graph.nodes.get(&id.0) else {
            continue;
        };
        if component.state == NodeState::Suppressed {
            continue;
        }
        let mut body = Body::new(&component.name, &id.0);
        for face in &solid.faces {
            if !face.indices.is_empty() {
                body.mesh.append_brep_face(face).with_context(|| {
                    format!("cannot read mesh for external component `{id}` face `{}`", face.face_id)
                })?;
            }
        }
        if !body.mesh.triangles.is_empty() {
            bodies.push(body);
        }
    }
    ensure!(
        !bodies.is_empty(),
        "project graph has no evaluated mesh bodies"
    );
    Ok((bodies, evaluator.merkle_cache().cache_hits()))
}

fn storage_is_sqlite(path: &Path) -> bool {
    matches!(
        cad_container::detect_storage_kind(path),
        Ok(cad_container::StorageKind::SqliteContainer)
    )
}

fn is_mesh_type(output_type: IrType) -> bool {
    matches!(output_type, IrType::Solid | IrType::Surface | IrType::SubD)
}

fn require_valid(id: &NodeId, report: &GraphEvalReport) -> Result<()> {
    let node = report
        .node_reports
        .get(id)
        .with_context(|| format!("node `{id}` was not evaluated"))?;
    ensure!(
        node.status == NodeState::Valid,
        "node `{id}` failed evaluation: {}",
        node.error
            .as_deref()
            .unwrap_or("node did not produce valid geometry")
    );
    Ok(())
}

fn component_transform(
    graph: &CanonicalGraph,
    node: &IrNode,
    report: &GraphEvalReport,
) -> Result<Mat4> {
    let pose = report
        .assembly_solve_report
        .as_ref()
        .and_then(|solve| solve.solved_poses.get(&node.id.0));
    let (origin, rotation) = match pose {
        Some(pose) => (pose.translation_mm, pose.rotation_deg),
        None => (
            input_vec3(graph, node, "origin")?.context("component has no origin")?,
            input_vec3(graph, node, "rotation")?.unwrap_or([0.0; 3]),
        ),
    };
    let [x, y, z] = origin;
    let [rx, ry, rz] = rotation;
    Ok(Mat4::translate(x, y, z).mul_mat(Mat4::rotate_euler_deg(rx, ry, rz)))
}

fn mesh_bodies(node: &IrNode, report: &GraphEvalReport) -> Result<Vec<Body>> {
    let id = &node.id;
    require_valid(id, report)?;
    let mut bodies = BTreeMap::new();
    if let Some(output) = report.output_implicit_meshes.get(id) {
        let mesh = &output.mesh;
        let (triangles, remainder) = mesh.indices.as_chunks::<3>();
        ensure!(
            remainder.is_empty(),
            "implicit mesh `{id}` has incomplete triangles"
        );
        let mut body = Body::new(&node.name, &id.0);
        body.mesh
            .append_face(&mesh.vertices, &mesh.normals, triangles)?;
        bodies.insert(None, body);
    } else if let Some(mesh) = report.output_subd_meshes.get(id) {
        let mut body = Body::new(&node.name, &id.0);
        body.mesh
            .append_face(&mesh.vertices, &mesh.normals, &mesh.triangles)?;
        bodies.insert(None, body);
    } else if let Some(solid) = report.output_solids.get(id) {
        let parts: BTreeMap<_, _> = solid
            .components
            .iter()
            .map(|part| (part.component_id.as_str(), part.part_name.as_str()))
            .collect();
        for face in &solid.faces {
            if face.indices.is_empty() {
                continue;
            }
            let body = bodies
                .entry(face.component_id.as_deref())
                .or_insert_with(|| match face.component_id.as_deref() {
                    None => Body::new(&node.name, &id.0),
                    Some(id) => Body::new(id, parts.get(id).copied().unwrap_or(id)),
                });
            body.mesh.append_brep_face(face).with_context(|| {
                format!("cannot read mesh for node `{id}` face `{}`", face.face_id)
            })?;
        }
    }
    ensure!(
        !bodies.is_empty() && bodies.values().all(|body| !body.mesh.triangles.is_empty()),
        "node `{id}` has no evaluated mesh triangles"
    );
    Ok(bodies.into_values().collect())
}

struct Body {
    name: String,
    part_number: String,
    mesh: Mesh,
}

impl Body {
    fn new(name: &str, part_number: &str) -> Self {
        Self {
            name: name.to_owned(),
            part_number: part_number.to_owned(),
            mesh: Mesh::default(),
        }
    }

    fn tree_node(&self, id: u32, bounds: Bounds) -> AssemblyTreeNode {
        AssemblyTreeNode {
            instance_id: id,
            parent_id: None,
            prototype_id: id,
            name: self.name.clone(),
            part_number: self.part_number.clone(),
            bbox_min_mm: bounds.min,
            bbox_max_mm: bounds.max,
            frozen_lightweight: true,
        }
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    fn include(&mut self, point: Point) {
        for (axis, coordinate) in point.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(coordinate);
            self.max[axis] = self.max[axis].max(coordinate);
        }
    }
}

#[derive(Clone, Copy)]
struct Vertex {
    position: Point,
    normal: Point,
    face_slot: u32,
}

#[derive(Default)]
struct Mesh {
    vertices: Vec<Vertex>,
    triangles: Vec<[u32; 3]>,
    face_count: u32,
}

impl Mesh {
    fn append_brep_face(&mut self, face: &BRepFace) -> Result<()> {
        let (positions, position_tail) = face.vertices.as_chunks::<3>();
        let (normals, normal_tail) = face.normals.as_chunks::<3>();
        let (triangles, triangle_tail) = face.indices.as_chunks::<3>();
        ensure!(
            position_tail.is_empty() && normal_tail.is_empty() && triangle_tail.is_empty(),
            "incomplete vertex or triangle triples"
        );
        if normals.is_empty() {
            return self.append_face(
                positions,
                &vec![face.normal_hint; positions.len()],
                triangles,
            );
        }
        self.append_face(positions, normals, triangles)
    }

    fn append_face(
        &mut self,
        positions: &[Point],
        normals: &[Point],
        triangles: &[[u32; 3]],
    ) -> Result<()> {
        ensure!(
            positions.len() == normals.len(),
            "mesh vertex and normal counts differ"
        );
        ensure!(
            positions.iter().chain(normals).copied().all(finite_point),
            "mesh contains non-finite coordinates or normals"
        );
        ensure!(
            triangles
                .iter()
                .flatten()
                .all(|&index| (index as usize) < positions.len()),
            "mesh triangle references a missing vertex"
        );
        let base = u32::try_from(self.vertices.len()).context("too many mesh vertices")?;
        u32::try_from(self.vertices.len().saturating_add(positions.len()))
            .context("too many mesh vertices")?;
        let face_slot = self.face_count;
        self.face_count = face_slot
            .checked_add(1)
            .context("too many mesh face slots")?;
        self.vertices.extend(
            positions
                .iter()
                .zip(normals)
                .map(|(&position, &normal)| Vertex {
                    position,
                    normal,
                    face_slot,
                }),
        );
        self.triangles.extend(
            triangles
                .iter()
                .map(|triangle| triangle.map(|index| base + index)),
        );
        Ok(())
    }

    fn bounds(&self) -> Bounds {
        let mut bounds = Bounds::empty();
        for vertex in &self.vertices {
            bounds.include(vertex.position);
        }
        bounds
    }

    fn hlod(
        &self,
        prototype_id: u32,
        level: HlodLevel,
        bounds: Bounds,
        divisions: Option<u32>,
    ) -> Result<PrototypeHlodChunk> {
        let mut chunk = PrototypeHlodChunk {
            prototype_id,
            level,
            anchor_mm: std::array::from_fn(|axis| bounds.min[axis].midpoint(bounds.max[axis])),
            vertices_local: Vec::new(),
            normals: Vec::new(),
            face_slot_ids: Vec::new(),
            indices: Vec::new(),
        };
        if let Some(divisions) = divisions {
            self.cluster_vertices(&mut chunk, bounds, divisions)?;
        }
        if chunk.indices.is_empty() {
            chunk.vertices_local.clear();
            chunk.normals.clear();
            chunk.face_slot_ids.clear();
            for &vertex in &self.vertices {
                push_vertex(&mut chunk, vertex, vertex.position)?;
            }
            chunk
                .indices
                .extend(self.triangles.iter().flatten().copied());
        }
        Ok(chunk)
    }

    fn cluster_vertices(
        &self,
        chunk: &mut PrototypeHlodChunk,
        bounds: Bounds,
        divisions: u32,
    ) -> Result<()> {
        // Snap to a shared grid while keeping normals and face IDs split at
        // topological seams. Nested grids make successive levels coarser.
        let grid_size = f64::from(divisions);
        let extents: Point = std::array::from_fn(|axis| bounds.max[axis] - bounds.min[axis]);
        let cells: Vec<[u32; 3]> = self
            .vertices
            .iter()
            .map(|vertex| {
                std::array::from_fn(|axis| {
                    if extents[axis] > 0.0 && extents[axis].is_finite() {
                        ((vertex.position[axis] - bounds.min[axis]) / extents[axis] * grid_size)
                            .round() as u32
                    } else {
                        0
                    }
                })
            })
            .collect();
        let mut slots = HashMap::new();
        for &triangle in &self.triangles {
            let [a, b, c] = triangle.map(|index| cells[index as usize]);
            if a == b || b == c || a == c {
                continue;
            }
            for (index, cell) in triangle.into_iter().zip([a, b, c]) {
                let vertex = self.vertices[index as usize];
                let slot = match slots.entry((cell, vertex.face_slot)) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let position = std::array::from_fn(|axis| {
                            bounds.min[axis] + f64::from(cell[axis]) / grid_size * extents[axis]
                        });
                        *entry.insert(push_vertex(chunk, vertex, position)?)
                    }
                };
                chunk.indices.push(slot);
            }
        }
        Ok(())
    }
}

fn finite_point(point: Point) -> bool {
    point.into_iter().all(f64::is_finite)
}

fn push_vertex(chunk: &mut PrototypeHlodChunk, vertex: Vertex, position: Point) -> Result<u32> {
    let local = std::array::from_fn(|axis| (position[axis] - chunk.anchor_mm[axis]) as f32);
    let normal = vertex.normal.map(|coordinate| coordinate as f32);
    ensure!(
        local.into_iter().chain(normal).all(f32::is_finite),
        "mesh coordinates or normals exceed the cadpack precision range"
    );
    let index =
        u32::try_from(chunk.vertices_local.len()).context("too many cadpack mesh vertices")?;
    chunk.vertices_local.push(local);
    chunk.normals.push(normal);
    chunk.face_slot_ids.push(vertex.face_slot);
    Ok(index)
}

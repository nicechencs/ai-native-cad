//! Node-level Merkle geometry snapshot cache (`MerkleEvalCache`) with `(merkle_hash, KernelFingerprint)`
//! lookup, hit/miss telemetry, and persistence bridge to `SqliteContainer::node_brep_cache`.

use std::collections::{BTreeMap, BTreeSet};

use cad_container::{ContainerError, KernelFingerprint, SqliteContainer};
use cad_ir::{CanonicalGraph, NodeId, compute_node_merkle_hashes};
use cad_kernel_occt::{
    BBox3, BRepEdge, BRepFace, ComponentMeta, CsgNode, KernelPayload, Mat4, MateRecord, PmiRecord,
    SketchPlane, SolidShape,
};
use cad_topo::{CandidateEntity, EvolutionRecord, GeometricFingerprint, TopoEntityId, TopoSnapshot};
use serde::{Deserialize, Serialize};

const CACHE_CODEC_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct MerkleCacheKey {
    merkle_hash: String,
    kernel_id: String,
    kernel_version: String,
}

impl MerkleCacheKey {
    fn new(merkle_hash: &str, kernel: &KernelFingerprint) -> Self {
        Self {
            merkle_hash: merkle_hash.to_ascii_lowercase(),
            kernel_id: kernel.kernel_id.clone(),
            kernel_version: kernel.kernel_version.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedNodeEntry {
    origin_node_id: String,
    solid: SolidShape,
    topo_snapshot: TopoSnapshot,
}

/// Cross-evaluation and cross-branch node-level geometry cache keyed by `(merkle_hash, KernelFingerprint)`.
#[derive(Debug, Clone, Default)]
pub struct MerkleEvalCache {
    entries: BTreeMap<MerkleCacheKey, CachedNodeEntry>,
    cache_hits: usize,
    cache_misses: usize,
}

impl MerkleEvalCache {
    /// Create an empty node-level Merkle geometry cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of `(merkle_hash, kernel)` lookups that hit a cached snapshot.
    pub const fn cache_hits(&self) -> usize {
        self.cache_hits
    }

    /// Total number of `(merkle_hash, kernel)` lookups that missed.
    pub const fn cache_misses(&self) -> usize {
        self.cache_misses
    }

    /// Reset hit and miss counters without evicting cached geometry.
    pub fn reset_stats(&mut self) {
        self.cache_hits = 0;
        self.cache_misses = 0;
    }

    /// Record an external/in-memory cache hit in telemetry counters.
    pub(crate) fn record_hit(&mut self) {
        self.cache_hits += 1;
    }

    /// Number of unique `(merkle_hash, kernel)` geometry snapshots currently resident.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if no cached geometry snapshots are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Check whether `(merkle_hash, kernel)` is present without updating hit/miss counters.
    pub fn contains(&self, merkle_hash: &str, kernel: &KernelFingerprint) -> bool {
        self.entries
            .contains_key(&MerkleCacheKey::new(merkle_hash, kernel))
    }

    /// Peek a cached `(SolidShape, TopoSnapshot)` adapted for `target_node_id` without mutating stats.
    pub fn peek(
        &self,
        merkle_hash: &str,
        kernel: &KernelFingerprint,
        target_node_id: &NodeId,
        preserve_creator: bool,
    ) -> Option<(SolidShape, TopoSnapshot)> {
        let entry = self.entries.get(&MerkleCacheKey::new(merkle_hash, kernel))?;
        let topo = adapt_topo_snapshot(
            &entry.topo_snapshot,
            &entry.origin_node_id,
            &target_node_id.0,
            preserve_creator,
        );
        Some((entry.solid.clone(), topo))
    }

    /// Look up `(merkle_hash, kernel)`, updating `cache_hits` or `cache_misses`.
    pub fn get(
        &mut self,
        merkle_hash: &str,
        kernel: &KernelFingerprint,
        target_node_id: &NodeId,
        preserve_creator: bool,
    ) -> Option<(SolidShape, TopoSnapshot)> {
        if let Some(found) = self.peek(merkle_hash, kernel, target_node_id, preserve_creator) {
            self.cache_hits += 1;
            Some(found)
        } else {
            self.cache_misses += 1;
            None
        }
    }

    /// Insert or update a node snapshot under `(merkle_hash, kernel)`.
    pub fn insert(
        &mut self,
        merkle_hash: String,
        kernel: &KernelFingerprint,
        origin_node_id: &NodeId,
        solid: SolidShape,
        topo_snapshot: TopoSnapshot,
    ) {
        let key = MerkleCacheKey::new(&merkle_hash, kernel);
        self.entries.insert(
            key,
            CachedNodeEntry {
                origin_node_id: origin_node_id.0.clone(),
                solid,
                topo_snapshot,
            },
        );
    }

    /// Encode a cached node snapshot into deterministic portable bytes suitable for `SqliteContainer::node_brep_cache`.
    /// Returns `Ok(None)` if `solid` carries an in-process native FFI guard that cannot be serialized portably.
    pub fn encode_snapshot_bytes(
        origin_node_id: &str,
        solid: &SolidShape,
        topo_snapshot: &TopoSnapshot,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(wire_solid) = WireSolidShape::from_solid(solid) else {
            return Ok(None);
        };
        let payload = WireCachedNodeSnapshot {
            codec_version: CACHE_CODEC_VERSION,
            origin_node_id: origin_node_id.to_owned(),
            solid: wire_solid,
            topo_snapshot: WireTopoSnapshot::from_topo(topo_snapshot),
        };
        serde_json::to_vec(&payload)
            .map(Some)
            .map_err(|err| err.to_string())
    }

    /// Decode portable bytes from `SqliteContainer::node_brep_cache` back into `(origin_node_id, SolidShape, TopoSnapshot)`.
    pub fn decode_snapshot_bytes(
        bytes: &[u8],
    ) -> Result<(String, SolidShape, TopoSnapshot), String> {
        let decoded: WireCachedNodeSnapshot =
            serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
        if decoded.codec_version != CACHE_CODEC_VERSION {
            return Err(format!(
                "unsupported cached node snapshot version {} (expected {})",
                decoded.codec_version, CACHE_CODEC_VERSION
            ));
        }
        Ok((
            decoded.origin_node_id,
            decoded.solid.into_solid(),
            decoded.topo_snapshot.into_topo(),
        ))
    }

    /// Hydrate matching `(merkle_hash, kernel)` entries for `graph` from `SqliteContainer::node_brep_cache`.
    /// Returns the number of newly loaded node snapshots.
    pub fn load_from_sqlite(
        &mut self,
        container: &SqliteContainer,
        graph: &CanonicalGraph,
        kernel: &KernelFingerprint,
    ) -> Result<usize, ContainerError> {
        let hashes = compute_node_merkle_hashes(graph)?;
        self.load_hashes_from_sqlite(container, &hashes, kernel)
    }

    pub(crate) fn load_hashes_from_sqlite(
        &mut self,
        container: &SqliteContainer,
        hashes: &BTreeMap<NodeId, String>,
        kernel: &KernelFingerprint,
    ) -> Result<usize, ContainerError> {
        let mut loaded = 0_usize;
        let mut seen_hashes = BTreeSet::new();
        for merkle_hash in hashes.values() {
            if !seen_hashes.insert(merkle_hash.as_str()) || self.contains(merkle_hash, kernel) {
                continue;
            }
            if let Some(bytes) = container.get_node_brep_cache(merkle_hash, kernel)?
                && let Ok((origin_node_id, solid, topo_snapshot)) =
                    Self::decode_snapshot_bytes(&bytes)
            {
                self.insert(
                    merkle_hash.clone(),
                    kernel,
                    &NodeId(origin_node_id),
                    solid,
                    topo_snapshot,
                );
                loaded += 1;
            }
        }
        Ok(loaded)
    }

    /// Persist resident `(merkle_hash, kernel)` entries referenced by `graph` into `SqliteContainer::node_brep_cache`.
    /// Declared `graph.outputs` are marked `is_output_pinned = true`. Returns the number of written entries.
    pub fn flush_to_sqlite(
        &self,
        container: &mut SqliteContainer,
        graph: &CanonicalGraph,
        kernel: &KernelFingerprint,
    ) -> Result<usize, ContainerError> {
        let hashes = compute_node_merkle_hashes(graph)?;
        let pinned_outputs: BTreeSet<&NodeId> = graph.outputs.iter().collect();
        self.flush_hashes_to_sqlite(container, &hashes, &pinned_outputs, kernel)
    }

    pub(crate) fn flush_hashes_to_sqlite(
        &self,
        container: &mut SqliteContainer,
        hashes: &BTreeMap<NodeId, String>,
        pinned_outputs: &BTreeSet<&NodeId>,
        kernel: &KernelFingerprint,
    ) -> Result<usize, ContainerError> {
        let mut written = 0_usize;
        for (node_id, merkle_hash) in hashes {
            let key = MerkleCacheKey::new(merkle_hash, kernel);
            let Some(entry) = self.entries.get(&key) else {
                continue;
            };
            let is_pinned = pinned_outputs.contains(node_id);
            let already_cached = container
                .get_node_brep_cache_entry(merkle_hash, kernel)?
                .is_some_and(|existing| existing.is_output_pinned || !is_pinned);
            if already_cached {
                continue;
            }
            if let Ok(Some(bytes)) = Self::encode_snapshot_bytes(
                &entry.origin_node_id,
                &entry.solid,
                &entry.topo_snapshot,
            ) {
                container.put_node_brep_cache_pinned(merkle_hash, kernel, &bytes, is_pinned)?;
                written += 1;
            }
        }
        Ok(written)
    }
}

fn adapt_topo_snapshot(
    source: &TopoSnapshot,
    origin_node_id: &str,
    target_node_id: &str,
    preserve_creator: bool,
) -> TopoSnapshot {
    if origin_node_id == target_node_id {
        return source.clone();
    }
    let candidates = source
        .candidates
        .iter()
        .map(|cand| {
            let mut updated = cand.clone();
            if !preserve_creator && updated.creator_node_id == origin_node_id {
                updated.creator_node_id = target_node_id.to_owned();
                updated.entity_id =
                    TopoEntityId(format!("{target_node_id}:{}", updated.local_output_slot));
                for record in &mut updated.evolution_chain {
                    if record.step_node_id == origin_node_id {
                        record.step_node_id = target_node_id.to_owned();
                    }
                }
            }
            updated
        })
        .collect();
    TopoSnapshot::new(candidates)
}

#[derive(Serialize, Deserialize)]
struct WireCachedNodeSnapshot {
    codec_version: u32,
    origin_node_id: String,
    solid: WireSolidShape,
    topo_snapshot: WireTopoSnapshot,
}

#[derive(Serialize, Deserialize)]
enum WirePayloadKind {
    Analytic,
    TriMesh,
}

#[derive(Serialize, Deserialize)]
struct WireSolidShape {
    payload: WirePayloadKind,
    part_name: String,
    csg: WireCsgNode,
    faces: Vec<BRepFace>,
    edges: Vec<BRepEdge>,
    pmi: Vec<PmiRecord>,
    exact_brep: bool,
    components: Vec<ComponentMeta>,
    mates: Vec<MateRecord>,
}

impl WireSolidShape {
    fn from_solid(solid: &SolidShape) -> Option<Self> {
        let payload = match &solid.payload {
            KernelPayload::Analytic => WirePayloadKind::Analytic,
            KernelPayload::TriMesh => WirePayloadKind::TriMesh,
            KernelPayload::Occt(_) => return None,
        };
        let csg = WireCsgNode::from_csg(&solid.csg)?;
        Some(Self {
            payload,
            part_name: solid.part_name.clone(),
            csg,
            faces: solid.faces.clone(),
            edges: solid.edges.clone(),
            pmi: solid.pmi.clone(),
            exact_brep: solid.exact_brep,
            components: solid.components.clone(),
            mates: solid.mates.clone(),
        })
    }

    fn into_solid(self) -> SolidShape {
        SolidShape {
            payload: match self.payload {
                WirePayloadKind::Analytic => KernelPayload::Analytic,
                WirePayloadKind::TriMesh => KernelPayload::TriMesh,
            },
            part_name: self.part_name,
            csg: self.csg.into_csg(),
            faces: self.faces,
            edges: self.edges,
            pmi: self.pmi,
            exact_brep: self.exact_brep,
            components: self.components,
            mates: self.mates,
        }
    }
}

#[derive(Serialize, Deserialize)]
enum WireCsgNode {
    Empty,
    Box {
        min: [f64; 3],
        max: [f64; 3],
    },
    ChamferedBox {
        min: [f64; 3],
        max: [f64; 3],
        chamfer_d: f64,
    },
    Cylinder {
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
        height: f64,
    },
    Counterbore {
        origin: [f64; 3],
        r_through: f64,
        r_cbore: f64,
        depth_through: f64,
        depth_cbore: f64,
    },
    Prism {
        plane: SketchPlane,
        polygon_2d: Vec<[f64; 2]>,
        offset_min: f64,
        offset_max: f64,
    },
    Revolve {
        plane: SketchPlane,
        polygon_2d: Vec<[f64; 2]>,
        axis: [f64; 3],
        angle_deg: f64,
    },
    Union(Vec<WireCsgNode>),
    Subtract {
        base: Box<WireCsgNode>,
        tools: Vec<WireCsgNode>,
    },
    Transform {
        child: Box<WireCsgNode>,
        fwd: [[f64; 4]; 4],
        inv: [[f64; 4]; 4],
    },
    Reflect {
        child: Box<WireCsgNode>,
        plane: SketchPlane,
    },
    Mesh {
        bbox: BBox3,
        triangles: Vec<[[f64; 3]; 3]>,
        closed: bool,
    },
}

impl WireCsgNode {
    fn from_csg(csg: &CsgNode) -> Option<Self> {
        Some(match csg {
            CsgNode::Empty => Self::Empty,
            CsgNode::Box { min, max } => Self::Box {
                min: *min,
                max: *max,
            },
            CsgNode::ChamferedBox {
                min,
                max,
                chamfer_d,
            } => Self::ChamferedBox {
                min: *min,
                max: *max,
                chamfer_d: *chamfer_d,
            },
            CsgNode::Cylinder {
                origin,
                axis,
                radius,
                height,
            } => Self::Cylinder {
                origin: *origin,
                axis: *axis,
                radius: *radius,
                height: *height,
            },
            CsgNode::Counterbore {
                origin,
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
            } => Self::Counterbore {
                origin: *origin,
                r_through: *r_through,
                r_cbore: *r_cbore,
                depth_through: *depth_through,
                depth_cbore: *depth_cbore,
            },
            CsgNode::Prism {
                plane,
                polygon_2d,
                offset_min,
                offset_max,
            } => Self::Prism {
                plane: *plane,
                polygon_2d: polygon_2d.clone(),
                offset_min: *offset_min,
                offset_max: *offset_max,
            },
            CsgNode::Revolve {
                plane,
                polygon_2d,
                axis,
                angle_deg,
            } => Self::Revolve {
                plane: *plane,
                polygon_2d: polygon_2d.clone(),
                axis: *axis,
                angle_deg: *angle_deg,
            },
            CsgNode::Union(items) => {
                Self::Union(items.iter().map(Self::from_csg).collect::<Option<Vec<_>>>()?)
            }
            CsgNode::Subtract { base, tools } => Self::Subtract {
                base: Box::new(Self::from_csg(base)?),
                tools: tools.iter().map(Self::from_csg).collect::<Option<Vec<_>>>()?,
            },
            CsgNode::Transform { child, fwd, inv } => Self::Transform {
                child: Box::new(Self::from_csg(child)?),
                fwd: fwd.m,
                inv: inv.m,
            },
            CsgNode::Reflect { child, plane } => Self::Reflect {
                child: Box::new(Self::from_csg(child)?),
                plane: *plane,
            },
            CsgNode::Mesh {
                bbox,
                triangles,
                closed,
            } => Self::Mesh {
                bbox: *bbox,
                triangles: triangles.clone(),
                closed: *closed,
            },
            CsgNode::Occt { .. } => return None,
        })
    }

    fn into_csg(self) -> CsgNode {
        match self {
            Self::Empty => CsgNode::Empty,
            Self::Box { min, max } => CsgNode::Box { min, max },
            Self::ChamferedBox {
                min,
                max,
                chamfer_d,
            } => CsgNode::ChamferedBox {
                min,
                max,
                chamfer_d,
            },
            Self::Cylinder {
                origin,
                axis,
                radius,
                height,
            } => CsgNode::Cylinder {
                origin,
                axis,
                radius,
                height,
            },
            Self::Counterbore {
                origin,
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
            } => CsgNode::Counterbore {
                origin,
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
            },
            Self::Prism {
                plane,
                polygon_2d,
                offset_min,
                offset_max,
            } => CsgNode::Prism {
                plane,
                polygon_2d,
                offset_min,
                offset_max,
            },
            Self::Revolve {
                plane,
                polygon_2d,
                axis,
                angle_deg,
            } => CsgNode::Revolve {
                plane,
                polygon_2d,
                axis,
                angle_deg,
            },
            Self::Union(items) => {
                CsgNode::Union(items.into_iter().map(Self::into_csg).collect())
            }
            Self::Subtract { base, tools } => CsgNode::Subtract {
                base: Box::new(base.into_csg()),
                tools: tools.into_iter().map(Self::into_csg).collect(),
            },
            Self::Transform { child, fwd, inv } => CsgNode::Transform {
                child: Box::new(child.into_csg()),
                fwd: Mat4 { m: fwd },
                inv: Mat4 { m: inv },
            },
            Self::Reflect { child, plane } => CsgNode::Reflect {
                child: Box::new(child.into_csg()),
                plane,
            },
            Self::Mesh {
                bbox,
                triangles,
                closed,
            } => CsgNode::Mesh {
                bbox,
                triangles,
                closed,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct WireTopoSnapshot {
    candidates: Vec<WireCandidateEntity>,
}

impl WireTopoSnapshot {
    fn from_topo(topo: &TopoSnapshot) -> Self {
        Self {
            candidates: topo
                .candidates
                .iter()
                .map(WireCandidateEntity::from_candidate)
                .collect(),
        }
    }

    fn into_topo(self) -> TopoSnapshot {
        TopoSnapshot::new(
            self.candidates
                .into_iter()
                .map(WireCandidateEntity::into_candidate)
                .collect(),
        )
    }
}

#[derive(Serialize, Deserialize)]
struct WireCandidateEntity {
    entity_id: TopoEntityId,
    creator_node_id: String,
    local_output_slot: String,
    evolution_chain: Vec<EvolutionRecord>,
    fingerprint: GeometricFingerprint,
    provenance_tags: Vec<String>,
}

impl WireCandidateEntity {
    fn from_candidate(cand: &CandidateEntity) -> Self {
        Self {
            entity_id: cand.entity_id.clone(),
            creator_node_id: cand.creator_node_id.clone(),
            local_output_slot: cand.local_output_slot.clone(),
            evolution_chain: cand.evolution_chain.clone(),
            fingerprint: cand.fingerprint.clone(),
            provenance_tags: cand.provenance_tags.clone(),
        }
    }

    fn into_candidate(self) -> CandidateEntity {
        CandidateEntity {
            entity_id: self.entity_id,
            creator_node_id: self.creator_node_id,
            local_output_slot: self.local_output_slot,
            evolution_chain: self.evolution_chain,
            fingerprint: self.fingerprint,
            provenance_tags: self.provenance_tags,
        }
    }
}

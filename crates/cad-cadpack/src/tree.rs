//! A flat, paginatable representation of a virtual assembly tree.
//!
//! The index deliberately keeps the insertion order of the source assembly.
//! This makes page cursors stable while a caller is walking the tree and also
//! gives deterministic results for search and ROI queries.

use serde::{Deserialize, Serialize};

/// A single lightweight assembly instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AssemblyTreeNode {
    pub instance_id: u32,
    pub parent_id: Option<u32>,
    pub prototype_id: u32,
    pub name: String,
    pub part_number: String,
    pub bbox_min_mm: [f64; 3],
    pub bbox_max_mm: [f64; 3],
    pub frozen_lightweight: bool,
}

/// A borrowed page from a [`VirtualAssemblyIndex`].
///
/// The node slice borrows the index, so obtaining a page does not clone the
/// potentially large tree. A page operation only constructs this small view
/// and takes O(1) time. Iterating the returned slice is O(limit).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TreePageSlice<'a> {
    pub offset: usize,
    pub total: usize,
    pub nodes: &'a [AssemblyTreeNode],
}

impl<'a> TreePageSlice<'a> {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// An in-memory, flat virtual assembly tree index.
///
/// Names are lower-cased once at construction time. This keeps repeated
/// case-insensitive search independent of the size of each node's original
/// name and avoids allocating a lowercase query for every node.
#[derive(Clone, Debug, Default)]
pub struct VirtualAssemblyIndex {
    nodes: Vec<AssemblyTreeNode>,
    normalized_names: Vec<String>,
    spatial_blocks: Vec<SpatialBlock>,
}

const SPATIAL_BLOCK_SIZE: usize = 64;

#[derive(Clone, Debug)]
struct SpatialBlock {
    start: usize,
    end: usize,
    bbox_min_mm: [f64; 3],
    bbox_max_mm: [f64; 3],
}

impl VirtualAssemblyIndex {
    /// Construct an index while preserving the supplied node order.
    pub fn new(nodes: Vec<AssemblyTreeNode>) -> Self {
        let normalized_names = nodes.iter().map(|node| node.name.to_lowercase()).collect();
        let spatial_blocks = build_spatial_blocks(&nodes);
        Self {
            nodes,
            normalized_names,
            spatial_blocks,
        }
    }

    /// Number of nodes in this index.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Return a node by its insertion-order index.
    pub fn get(&self, index: usize) -> Option<&AssemblyTreeNode> {
        self.nodes.get(index)
    }

    /// Borrow all nodes in insertion order.
    pub fn nodes(&self) -> &[AssemblyTreeNode] {
        &self.nodes
    }

    /// Return one page without copying its nodes.
    ///
    /// Offsets beyond the end produce an empty page. `limit == 0` is valid and
    /// also produces an empty page. Arithmetic is intentionally performed with
    /// checked addition so a caller-supplied `usize::MAX` cannot wrap.
    pub fn page_slice(&self, offset: usize, limit: usize) -> TreePageSlice<'_> {
        let start = offset.min(self.nodes.len());
        let end = start
            .checked_add(limit)
            .map_or(self.nodes.len(), |candidate| {
                candidate.min(self.nodes.len())
            });
        TreePageSlice {
            offset,
            total: self.nodes.len(),
            nodes: &self.nodes[start..end],
        }
    }

    /// Find node IDs whose names contain `query`, case-insensitively.
    ///
    /// Results retain insertion order and are capped at `limit`. An empty query
    /// matches every node, which is useful for implementing an initial tree
    /// window without a separate code path.
    pub fn search_by_name(&self, query: &str, limit: usize) -> Vec<u32> {
        if limit == 0 {
            return Vec::new();
        }
        let query = query.to_lowercase();
        self.nodes
            .iter()
            .zip(self.normalized_names.iter())
            .filter(|(_, normalized)| normalized.contains(&query))
            .map(|(node, _)| node.instance_id)
            .take(limit)
            .collect()
    }

    /// Find node IDs whose axis-aligned boxes intersect an ROI.
    ///
    /// Intersection is inclusive, so touching faces, edges, or corners count
    /// as a match. Reversed or non-finite ROI endpoints return no matches.
    /// Nodes with reversed or non-finite endpoints are skipped. Results retain
    /// insertion order.
    pub fn query_roi_bbox(&self, roi_min: [f64; 3], roi_max: [f64; 3]) -> Vec<u32> {
        let mut matches = Vec::new();
        self.visit_roi_hits(roi_min, roi_max, |node| matches.push(node.instance_id));
        matches
    }

    /// Visit valid boxes intersecting an ROI in source insertion order and
    /// return the exact number of matches. This shared primitive lets digest
    /// generation retain node identity even if source data has duplicate IDs,
    /// without allocating a second list of hit positions.
    pub(crate) fn visit_roi_hits<F>(
        &self,
        roi_min: [f64; 3],
        roi_max: [f64; 3],
        mut visitor: F,
    ) -> usize
    where
        F: FnMut(&AssemblyTreeNode),
    {
        let Some((roi_min, roi_max)) = valid_bbox(roi_min, roi_max) else {
            return 0;
        };
        let mut matches = 0;
        for block in &self.spatial_blocks {
            if !bbox_intersects(block.bbox_min_mm, block.bbox_max_mm, roi_min, roi_max) {
                continue;
            }
            for position in block.start..block.end {
                let node = &self.nodes[position];
                let Some((node_min, node_max)) = valid_bbox(node.bbox_min_mm, node.bbox_max_mm)
                else {
                    continue;
                };
                if bbox_intersects(node_min, node_max, roi_min, roi_max) {
                    matches += 1;
                    visitor(node);
                }
            }
        }
        matches
    }
}

fn build_spatial_blocks(nodes: &[AssemblyTreeNode]) -> Vec<SpatialBlock> {
    let block_count = nodes.len().saturating_add(SPATIAL_BLOCK_SIZE - 1) / SPATIAL_BLOCK_SIZE;
    let mut blocks = Vec::with_capacity(block_count);
    for start in (0..nodes.len()).step_by(SPATIAL_BLOCK_SIZE) {
        let end = (start + SPATIAL_BLOCK_SIZE).min(nodes.len());
        let valid_boxes = nodes[start..end]
            .iter()
            .filter_map(|node| valid_bbox(node.bbox_min_mm, node.bbox_max_mm));
        let mut bbox_min_mm = [0.0; 3];
        let mut bbox_max_mm = [0.0; 3];
        let mut has_valid_box = false;
        for (node_min, node_max) in valid_boxes {
            if !has_valid_box {
                bbox_min_mm = node_min;
                bbox_max_mm = node_max;
                has_valid_box = true;
                continue;
            }
            for axis in 0..3 {
                bbox_min_mm[axis] = bbox_min_mm[axis].min(node_min[axis]);
                bbox_max_mm[axis] = bbox_max_mm[axis].max(node_max[axis]);
            }
        }
        if has_valid_box {
            blocks.push(SpatialBlock {
                start,
                end,
                bbox_min_mm,
                bbox_max_mm,
            });
        }
    }
    blocks
}

fn valid_bbox(min: [f64; 3], max: [f64; 3]) -> Option<([f64; 3], [f64; 3])> {
    if (0..3).any(|axis| !min[axis].is_finite() || !max[axis].is_finite() || min[axis] > max[axis])
    {
        return None;
    }
    Some((min, max))
}

fn bbox_intersects(
    first_min: [f64; 3],
    first_max: [f64; 3],
    second_min: [f64; 3],
    second_max: [f64; 3],
) -> bool {
    (0..3).all(|axis| first_max[axis] >= second_min[axis] && first_min[axis] <= second_max[axis])
}

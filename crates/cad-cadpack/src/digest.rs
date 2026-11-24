//! Compact JSON summaries for AI agents working on a spatial ROI.

use serde::{Deserialize, Serialize};

use crate::tree::VirtualAssemblyIndex;

/// Maximum serialized size of a generated interface digest.
pub const MAX_DIGEST_JSON_BYTES: usize = 12 * 1024;
const MAX_COMPONENTS: usize = 10;
// 32 raw bytes leave ample headroom for JSON escaping, six f64 coordinates,
// and object keys while keeping ten summaries well below 12 KiB.
const MAX_LABEL_BYTES: usize = 32;

/// A compact, tree-derived component summary.
///
/// The bounding box is the lightweight assembly box and does not claim that a
/// component's exact B-Rep has been loaded. Consumers can use
/// `frozen_lightweight` to decide whether a precise wake-up is needed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RoiComponentSummary {
    pub instance_id: u32,
    pub prototype_id: u32,
    pub name: String,
    pub part_number: String,
    pub bbox_min_mm: [f64; 3],
    pub bbox_max_mm: [f64; 3],
    pub frozen_lightweight: bool,
}

/// JSON-friendly context for a single ROI query.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RoiInterfaceDigest {
    pub schema_version: String,
    pub roi_min_mm: [f64; 3],
    pub roi_max_mm: [f64; 3],
    pub components: Vec<RoiComponentSummary>,
    pub total_matches: usize,
    /// True when a component label was shortened to keep this digest compact.
    pub labels_truncated: bool,
    /// True when either labels or matching components were omitted.
    pub truncated: bool,
}

impl RoiInterfaceDigest {
    /// Serialize this digest as compact JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serialize this digest as compact UTF-8 JSON bytes.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }
}

/// Build a deterministic, bounded JSON digest for components intersecting an
/// ROI. Components are retained in source insertion order and capped at ten,
/// which keeps the summary focused on the local neighborhood. Component labels
/// are capped at 32 UTF-8 bytes. These bounds keep generated JSON below 12 KiB
/// even with worst-case JSON escaping. `truncated` records either kind of
/// omission, while `labels_truncated` identifies label shortening specifically.
/// An invalid ROI (reversed or non-finite endpoint) produces an empty digest
/// with `total_matches == 0`. Its endpoint arrays retain the caller's values,
/// so `to_json` may return a serde error for non-finite values.
pub fn build_roi_interface_digest(
    index: &VirtualAssemblyIndex,
    roi_min: [f64; 3],
    roi_max: [f64; 3],
) -> RoiInterfaceDigest {
    let mut components = Vec::with_capacity(MAX_COMPONENTS);
    let mut labels_truncated = false;
    let total_matches = index.visit_roi_hits(roi_min, roi_max, |node| {
        if components.len() < MAX_COMPONENTS {
            let (name, name_was_truncated) = bounded_text_with_flag(&node.name);
            let (part_number, part_number_was_truncated) =
                bounded_text_with_flag(&node.part_number);
            labels_truncated |= name_was_truncated || part_number_was_truncated;
            components.push(RoiComponentSummary {
                instance_id: node.instance_id,
                prototype_id: node.prototype_id,
                name,
                part_number,
                bbox_min_mm: node.bbox_min_mm,
                bbox_max_mm: node.bbox_max_mm,
                frozen_lightweight: node.frozen_lightweight,
            });
        }
    });

    RoiInterfaceDigest {
        schema_version: "1.0".to_owned(),
        roi_min_mm: roi_min,
        roi_max_mm: roi_max,
        components,
        total_matches,
        labels_truncated,
        truncated: labels_truncated || total_matches > MAX_COMPONENTS,
    }
}

// Keep arbitrary imported names from dominating a digest. Truncating on a
// character boundary also guarantees valid UTF-8, including for multibyte
// component names.
fn bounded_text_with_flag(text: &str) -> (String, bool) {
    if text.len() <= MAX_LABEL_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_LABEL_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

//! Convert STEP XCAF records into the project container's engineering metadata.

use std::collections::{BTreeMap, HashMap, HashSet};

use cad_container::{
    AicadProject, AppearanceOverride, BomItem, EngineeringMetadata, PmiAnnotation, PmiKind,
};
use cad_kernel_occt::{XcafAssemblyNode, XcafStepMetadata};
use cad_topo::{EntityKind, QueryTopoRef, TopoFilter, TopoRef};

/// Convert XCAF colors, part occurrences, and PMI into engineering records.
pub fn xcaf_metadata_to_engineering(meta: &XcafStepMetadata) -> EngineeringMetadata {
    let appearances = meta
        .assembly_nodes
        .iter()
        .filter_map(|node| {
            let rgba = node.color_rgba?;
            Some(AppearanceOverride {
                target: xcaf_target(format!("xcaf:node:{}:{}", node.id, node.name)),
                rgba: rgba.map(|channel| channel as f32),
                metallic: 0.1,
                roughness: 0.45,
                finish_note: Some(format!("xcaf:{}", node.name)),
            })
        })
        .collect();

    let mut pmi_occurrences = HashMap::new();
    let pmi = meta
        .pmi_items
        .iter()
        .map(|item| {
            let key = (
                item.kind.as_str(),
                item.name.as_str(),
                item.value_text.as_str(),
            );
            let occurrence = pmi_occurrences.entry(key).or_insert(0usize);
            *occurrence += 1;
            let (kind, name, value) = key;
            let id = format!("xcaf:pmi:{kind:?}:{name:?}:{value:?}:{occurrence}");
            let value_text = value.trim();
            let name_hint = name.to_ascii_lowercase();
            let kind = match kind {
                "datum" => PmiKind::Datum,
                "geometric_tolerance" if name_hint.contains("flatness") => PmiKind::Flatness,
                "geometric_tolerance" if name_hint.contains("perpendicular") => {
                    PmiKind::Perpendicularity
                }
                "geometric_tolerance" => PmiKind::Position,
                _ => PmiKind::ToleranceBand,
            };
            let mut datum_refs = Vec::new();
            if kind == PmiKind::Datum {
                // The bridge puts the datum identification in value_text.
                let datum = if value_text.is_empty() {
                    name.trim()
                } else {
                    value_text
                };
                if !datum.is_empty() {
                    datum_refs.push(datum.to_owned());
                }
            }
            PmiAnnotation {
                target: xcaf_target(id.clone()),
                id,
                kind,
                label: name.to_owned(),
                value_mm: value_text.parse::<f64>().ok().filter(|num| num.is_finite()),
                datum_refs,
            }
        })
        .collect();

    EngineeringMetadata {
        appearances,
        bom: convert_bom(&meta.assembly_nodes),
        pmi,
    }
}

/// Append new XCAF records while preserving existing records with the same identity.
pub fn merge_xcaf_into_project(project: &mut AicadProject, meta: &XcafStepMetadata) {
    let mut imported = xcaf_metadata_to_engineering(meta);
    let engineering = &mut project.engineering;

    let pmi_ids: HashSet<_> = engineering.pmi.iter().map(|pmi| pmi.id.as_str()).collect();
    imported
        .pmi
        .retain(|pmi| !pmi_ids.contains(pmi.id.as_str()));
    engineering.pmi.extend(imported.pmi);

    let part_numbers: HashSet<_> = engineering
        .bom
        .iter()
        .map(|item| item.part_number.as_str())
        .collect();
    imported
        .bom
        .retain(|item| !part_numbers.contains(item.part_number.as_str()));
    engineering.bom.extend(imported.bom);

    for appearance in imported.appearances {
        if !engineering
            .appearances
            .iter()
            .any(|existing| existing.target == appearance.target)
        {
            engineering.appearances.push(appearance);
        }
    }
}

fn convert_bom(nodes: &[XcafAssemblyNode]) -> Vec<BomItem> {
    let parent_ids: HashSet<_> = nodes
        .iter()
        .filter_map(|node| node.parent_id.as_deref())
        .collect();
    let is_flat = parent_ids.is_empty();
    let mut occurrences: BTreeMap<String, Vec<&str>> = BTreeMap::new();

    for node in nodes {
        let name = node.name.trim();
        let is_leaf_part = !node.is_assembly && !parent_ids.contains(node.id.as_str());
        if (is_flat && name.is_empty()) || (!is_flat && !is_leaf_part) {
            continue;
        }
        // XCAF exposes occurrence names, but no separate part number or revision.
        let part_number = if name.is_empty() {
            format!("xcaf:node:{}", node.id)
        } else {
            name.to_owned()
        };
        occurrences.entry(part_number).or_default().push(&node.id);
    }

    occurrences
        .into_iter()
        .map(|(part_number, mut ids)| {
            ids.sort_unstable();
            BomItem {
                name: part_number.clone(),
                part_number,
                revision: String::new(),
                material_id: None,
                quantity: u32::try_from(ids.len()).unwrap_or(u32::MAX),
                custom_attributes: BTreeMap::from([("xcaf_node_ids".to_owned(), ids.join(","))]),
            }
        })
        .collect()
}

fn xcaf_target(tag: String) -> TopoRef {
    // XCAF metadata provides source identities without face indices or fingerprints.
    TopoRef::Query(QueryTopoRef {
        entity_kind: EntityKind::Face,
        filters: vec![TopoFilter::Tagged(tag)],
        tie_breakers: Vec::new(),
        require_unique: true,
    })
}

//! STEP XCAF metadata exchange for the project CLI.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use cad_container::{AicadProject, KclProjectionPolicy};
use cad_evaluator::merge_xcaf_into_project;
use cad_ir::{BlobHandle, BlobKind, IrNode, IrType, IrValue, NodeId, NodeKind, NodeState};
use cad_kcl_bridge::emit_kcl;
use cad_kernel_occt::xcaf::{
    XcafExportSpec, export_step_xcaf_metadata, serialize_xcaf_export_spec_json,
};
use cad_kernel_occt::{KernelError, import_step_xcaf_metadata, parse_xcaf_metadata_json};
use serde_json::json;

use crate::project::{CommandOutput, ProjectFormat, emit_error, emit_output};

struct MountedStepInfo {
    node_id: String,
    blob_handle: String,
    blob_sha256: String,
}

pub(super) fn run_import_step_xcaf(
    project_path: &Path,
    step: Option<&Path>,
    xcaf_json: Option<&Path>,
    mount_geometry: bool,
    replace_outputs: bool,
    json_out: bool,
) -> Result<ExitCode> {
    if replace_outputs && !mount_geometry {
        bail!("--replace-outputs requires --mount-geometry");
    }
    let metadata = match (step, xcaf_json) {
        (Some(path), None) => import_step_xcaf_metadata(path).with_context(|| {
            format!("cannot import STEP XCAF metadata from `{}`", path.display())
        })?,
        (None, Some(path)) => {
            if mount_geometry {
                bail!("--mount-geometry requires --step <FILE>");
            }
            let raw = fs::read_to_string(path)
                .with_context(|| format!("cannot read XCAF JSON `{}`", path.display()))?;
            parse_xcaf_metadata_json(&raw)
                .with_context(|| format!("cannot parse XCAF JSON `{}`", path.display()))?
        }
        _ => bail!("provide exactly one of --step or --xcaf-json"),
    };

    let storage = ProjectFormat::detect(project_path)?;
    let mut project = storage.load(project_path)?;
    merge_xcaf_into_project(&mut project, &metadata);

    let mounted = if mount_geometry {
        let step_path = step.expect("--mount-geometry requires --step");
        let info = mount_step_geometry(&mut project, step_path, replace_outputs)?;
        match project.manifest.kcl_projection_policy {
            KclProjectionPolicy::MaterializedSynced => {
                project.kcl_view =
                    Some(emit_kcl(&project.graph).context("cannot refresh KCL view")?);
            }
            KclProjectionPolicy::OnDemandDynamic => {
                project.kcl_view = None;
            }
        }
        Some(info)
    } else {
        None
    };

    storage.save(&project, project_path)?;

    let appearances = project.engineering.appearances.len();
    let bom = project.engineering.bom.len();
    let pmi = project.engineering.pmi.len();
    let stats = &metadata.stats;
    let mut text = format!(
        "Merged STEP XCAF metadata into {}\n\
         Project totals: {appearances} appearances, {bom} BOM entries, {pmi} PMI annotations\n\
         Source stats: {} shapes, {} colored shapes, {} PMI items",
        project_path.display(),
        stats.total_shapes,
        stats.colored_shapes,
        stats.pmi_count
    );
    let mut report = json!({
        "status": "ok",
        "path": project_path.to_string_lossy(),
        "appearances": appearances,
        "bom": bom,
        "pmi": pmi,
        "stats": stats,
    });
    if let Some(info) = mounted {
        text.push_str(&format!(
            "\nMounted STEP geometry as node `{}` (blob `{}`, sha256 `{}`)",
            info.node_id, info.blob_handle, info.blob_sha256
        ));
        if let Some(map) = report.as_object_mut() {
            map.insert("mounted_node_id".to_owned(), json!(info.node_id));
            map.insert("mounted_blob_handle".to_owned(), json!(info.blob_handle));
            map.insert("mounted_blob_sha256".to_owned(), json!(info.blob_sha256));
            map.insert("replaced_outputs".to_owned(), json!(replace_outputs));
        }
    }
    let output = CommandOutput::report(text, report);
    Ok(emit_output(output, json_out))
}

fn mount_step_geometry(
    project: &mut AicadProject,
    step_path: &Path,
    replace_outputs: bool,
) -> Result<MountedStepInfo> {
    let step_bytes = fs::read(step_path)
        .with_context(|| format!("cannot read STEP file `{}`", step_path.display()))?;
    let base_ident = sanitize_step_ident(step_path);
    let node_id_str = unique_key(&base_ident, |candidate| {
        project.graph.nodes.contains_key(candidate)
    });
    let blob_handle_str = unique_key(&format!("{node_id_str}_blob"), |candidate| {
        project.graph.blobs.contains_key(candidate)
    });
    let blob_handle = BlobHandle::from(blob_handle_str.clone());
    let entry = project
        .put_blob(blob_handle.clone(), BlobKind::Import, step_bytes)
        .with_context(|| format!("cannot store STEP blob from `{}`", step_path.display()))?;

    let safe_source_file = safe_source_filename(step_path);
    let display_name = step_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("Imported STEP")
        .to_owned();

    let mut inputs = BTreeMap::new();
    inputs.insert("blob".to_owned(), IrValue::BlobRef(blob_handle));
    inputs.insert("format".to_owned(), IrValue::String("step".to_owned()));
    inputs.insert("allow_open_shell".to_owned(), IrValue::Bool(true));

    let mut metadata = BTreeMap::new();
    metadata.insert("source_file".to_owned(), safe_source_file);
    metadata.insert("format".to_owned(), "step".to_owned());

    let node_id = NodeId::from(node_id_str.clone());
    project.graph.nodes.insert(
        node_id_str.clone(),
        IrNode {
            id: node_id.clone(),
            name: display_name,
            kind: NodeKind::ImportAsset,
            inputs,
            output_type: IrType::Solid,
            state: NodeState::Valid,
            kernel_op_version: None,
            metadata,
        },
    );

    if replace_outputs {
        project.graph.outputs = vec![node_id];
    } else if !project.graph.outputs.contains(&node_id) {
        project.graph.outputs.push(node_id);
    }

    Ok(MountedStepInfo {
        node_id: node_id_str,
        blob_handle: blob_handle_str,
        blob_sha256: entry.sha256,
    })
}

fn sanitize_step_ident(step_path: &Path) -> String {
    let raw = step_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("step");
    let mut slug = String::new();
    let mut prev_underscore = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !slug.is_empty() {
            slug.push('_');
            prev_underscore = true;
        }
    }
    let slug = slug.trim_matches('_');
    if slug.is_empty() {
        "imported_step".to_owned()
    } else {
        format!("imported_step_{slug}")
    }
}

fn safe_source_filename(step_path: &Path) -> String {
    let raw = step_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("imported.step");
    let sanitized: String = raw
        .chars()
        .filter(|ch| !ch.is_control() && !matches!(ch, '/' | '\\' | ':'))
        .collect();
    if sanitized.is_empty() {
        "imported.step".to_owned()
    } else {
        sanitized
    }
}

fn unique_key(base: &str, mut exists: impl FnMut(&str) -> bool) -> String {
    if !exists(base) {
        return base.to_owned();
    }
    let mut suffix = 2_usize;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !exists(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

pub(super) fn run_export_step_xcaf(
    spec_path: &Path,
    out_path: &Path,
    json_out: bool,
) -> Result<ExitCode> {
    let raw = fs::read_to_string(spec_path)
        .with_context(|| format!("cannot read XCAF export spec `{}`", spec_path.display()))?;
    let spec: XcafExportSpec = serde_json::from_str(&raw)
        .with_context(|| format!("cannot parse XCAF export spec `{}`", spec_path.display()))?;
    serialize_xcaf_export_spec_json(&spec)
        .with_context(|| format!("invalid XCAF export spec `{}`", spec_path.display()))?;

    let result = export_step_xcaf_metadata(out_path, &spec);
    if let Err(error @ KernelError::BackendUnavailable) = result {
        return Ok(emit_error(error, json_out));
    }
    result.with_context(|| format!("cannot export STEP XCAF to `{}`", out_path.display()))?;

    let part_count = spec.parts.len();
    let pmi_count = spec.pmi_items.len();
    let output = CommandOutput::report(
        format!(
            "Wrote STEP XCAF with {part_count} parts and {pmi_count} PMI items to {}",
            out_path.display()
        ),
        json!({
            "status": "ok",
            "out": out_path.to_string_lossy(),
            "part_count": part_count,
            "pmi_count": pmi_count,
        }),
    );
    Ok(emit_output(output, json_out))
}

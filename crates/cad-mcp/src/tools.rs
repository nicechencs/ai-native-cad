//! Eight orthogonal MCP tools. Schemas stay small and point at `aicad://`.

use std::path::Path;

use cad_api::{
    ContextSliceRequest, ExplainTarget, NodeId, NodeRecoveryHint, inspect_document_summary,
    slice_document_context,
};
use cad_runtime::{Actor, BranchTarget, DocumentSession, RuntimeMode, VerificationProfile};
use cad_topo::{
    CandidateEntity, EvolutionRecord, GeometricFingerprint, MatchTolerances, MeasurementKind,
    MeasurementOptions, TopoEntityId, TopoError, TopoRef, TopoSnapshot, TopologyQueryFilter,
    build_ambiguous_topo_hints, measure_geometry_refs, query_topology,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::GatewayError;
use crate::protocol::{estimate_tokens, tool_result};
use crate::sanitize::sanitize_untrusted_label;
use crate::verify::parse_profile;

pub use crate::protocol::estimate_tokens as estimate_token_budget;

const TOOL_NAMES: [&str; 8] = [
    "cad_session_context",
    "cad_inspect_and_slice",
    "cad_query_geometry",
    "cad_apply_commands",
    "cad_sync_kcl_view",
    "cad_verify_and_render",
    "cad_branch_control",
    "cad_export_artifacts",
];

pub fn tool_definitions() -> Value {
    json!([
        tool(
            "cad_session_context",
            "Read mode, project path, revision, branch, viewport selection, camera, and design contract.",
            object(json!({}), &[]),
        ),
        tool(
            "cad_inspect_and_slice",
            "Read a document summary or a focus slice. Schemas: aicad://schema/semantic-commands.",
            object(json!({
                "granularity": {"type": "string", "enum": ["summary", "slice"]},
                "focus_nodes": {"type": "array", "items": {"type": "string"}},
                "focus_topo_refs": {"type": "array", "items": {"type": "string"}},
                "upstream_depth": {"type": "integer"},
                "downstream_depth": {"type": "integer"}
            }), &[]),
        ),
        tool(
            "cad_query_geometry",
            "Read topology, measure, or explain. Measure kind volume or bounding_box needs no left/right and reads the finished solid. Distance kinds are CentroidDistance, ParallelPlaneDistance, Coaxiality, and AngleDeg; left and right must be {\"Lineage\": <topo_ref from a topology hit>} or {\"Query\": {\"entity_kind\":\"Face\",\"filters\":[{\"Surface\":\"Plane\"}],\"tie_breakers\":[],\"require_unique\":true}}. Query filters are single-key objects, not a flat map. Omitting snapshot uses the live session.",
            object(json!({
                "query": {"type": "string", "enum": ["topology", "measure", "explain"]},
                "filter": {"type": "object", "description": "Topology filter. Optional fields include entity_kind, surface_class, creator_node_id, normal_target, normal_angle_tol_deg, and limit."},
                "snapshot": {"type": "array", "items": {"type": "object"}},
                "left": {"type": "object", "description": "Required for distance kinds. {\"Lineage\": topo_ref} or {\"Query\": query}. Not used by volume or bounding_box."},
                "right": {"type": "object", "description": "Required for distance kinds, same shape as left."},
                "kind": {"type": "string", "enum": ["volume", "bounding_box", "CentroidDistance", "ParallelPlaneDistance", "Coaxiality", "AngleDeg"], "description": "volume and bounding_box do not take left/right. There is no kind named node_id."},
                "tolerances": {"type": "object"},
                "options": {"type": "object"},
                "target": {"type": "object", "description": "explain only. {\"target\":\"node\",\"node_id\":\"plate\"} or {\"target\":\"parameter\",\"name\":\"width\"}."}
            }), &["query"]),
        ),
        tool(
            "cad_apply_commands",
            "Atomic Vec<SemanticCommand>. Read aicad://schema/semantic-commands first. To change a literal width, depth, height, diameter, or origin on an existing box or hole, use set_feature_input; do not delete and recreate the node. If that input is bound to a parameter, use upsert_parameter instead. Returns ChangeSummary and revision.",
            object(json!({
                "expected_revision": {"type": "integer"},
                "actor": {
                    "type": "object",
                    "description": "Caller identity. kind=agent is invalid. External agents must send {\"kind\":\"external_agent\",\"client\":\"grok\",\"session_id\":\"<the real session id>\"}. Do not send human_ui unless this call comes from the CAD UI.",
                    "properties": {
                        "kind": {"type": "string", "enum": ["human_ui", "builtin_agent", "external_agent", "python_sdk", "kcl_view"]},
                        "client": {"type": "string", "description": "Required when kind is external_agent."},
                        "session_id": {"type": "string", "description": "Required when kind is external_agent. Use the caller's real session id."}
                    },
                    "required": ["kind"]
                },
                "branch_policy": {"type": "string", "description": "preview (default), active, main, commit_if_pass, or named:<branch>"},
                "commands": {"type": "array", "items": {"type": "object"}, "description": "See aicad://schema/feature/<kind>. set_feature_input args are {node_id, input, length_mm?} or {node_id, input, vector_mm?}."}
            }), &["expected_revision", "actor", "commands"]),
        ),
        tool(
            "cad_sync_kcl_view",
            "Emit the .kcl view, or sync edited .kcl with expected_revision.",
            object(json!({
                "action": {"type": "string", "enum": ["emit", "sync"]},
                "expected_revision": {"type": "integer"},
                "actor": {"type": "object"},
                "kcl": {"type": "string"}
            }), &["action"]),
        ),
        tool(
            "cad_verify_and_render",
            "L0-L4 tri-state report. profile is fast or release. Render pixels stay UNKNOWN.",
            object(json!({
                "profile": {"type": "string", "enum": ["fast", "release"]},
                "render": {"type": "boolean"},
                "views": {"type": "array", "items": {"type": "string", "enum": ["iso", "front", "top", "right"]}}
            }), &[]),
        ),
        tool(
            "cad_branch_control",
            "list, diff, request_review, commit_preview, discard_preview, undo, or redo.",
            object(json!({
                "action": {"type": "string"},
                "branch": {"type": "string"}
            }), &["action"]),
        ),
        tool(
            "cad_export_artifacts",
            "Write a real STEP AP242, 3MF, DXF, or SVG file inside the workspace. Rejects ../ and outside paths. part_label is the user-facing part name and resolves to the finished solid, even when that name is on an upstream feature. node_id exports that exact node, including an intermediate feature. A name that matches several finished solids is rejected. An unknown name is rejected and does not invent an alias.",
            object(json!({
                "format": {"type": "string"},
                "path": {"type": "string"},
                "part_label": {"type": "string", "description": "User-facing part name. Resolves to the finished solid. Case-insensitive. Does not silently export a consumed upstream feature."},
                "node_id": {"type": "string", "description": "Explicit node id. Exports that node even when a later feature consumed it."}
            }), &["format", "path"]),
        )
    ])
}

pub fn export_openai_tool_schemas() -> Value {
    let Value::Array(tools) = tool_definitions() else {
        return json!([]);
    };
    Value::Array(
        tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").cloned().unwrap_or(Value::Null),
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("inputSchema").cloned().unwrap_or(json!({})),
                    }
                })
            })
            .collect(),
    )
}

pub fn tools_list_result() -> Value {
    json!({ "tools": tool_definitions() })
}

pub fn token_budget_of_tools_list() -> usize {
    let raw = serde_json::to_string(&tools_list_result()).unwrap_or_default();
    estimate_tokens(&raw)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContextArgs {}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectAndSliceArgs {
    #[serde(default)]
    granularity: Option<String>,
    #[serde(default)]
    focus_nodes: Option<Vec<String>>,
    #[serde(default)]
    focus_topo_refs: Option<Vec<String>>,
    #[serde(default)]
    upstream_depth: Option<usize>,
    #[serde(default)]
    downstream_depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryGeometryArgs {
    query: String,
    #[serde(default)]
    filter: Option<TopologyQueryFilter>,
    #[serde(default)]
    snapshot: Option<Vec<WireCandidate>>,
    #[serde(default)]
    left: Option<Value>,
    #[serde(default)]
    right: Option<Value>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    tolerances: Option<MatchTolerances>,
    #[serde(default)]
    options: Option<MeasurementOptions>,
    #[serde(default)]
    target: Option<ExplainTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyCommandsArgs {
    expected_revision: u64,
    actor: Value,
    #[serde(default)]
    branch_policy: Option<String>,
    commands: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncKclViewArgs {
    action: String,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    actor: Option<Actor>,
    #[serde(default)]
    kcl: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyAndRenderArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    render: Option<bool>,
    #[serde(default)]
    views: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchControlArgs {
    action: String,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportArtifactsArgs {
    format: String,
    path: String,
    #[serde(default)]
    part_label: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, GatewayError> {
    serde_json::from_value(args.clone()).map_err(|err| GatewayError::invalid(err.to_string()))
}

fn attach_recovery_hints(body: &mut Value, hints: &[NodeRecoveryHint]) {
    if !hints.is_empty() {
        body["recovery_hints"] = json!(hints);
    }
}

pub fn call_tool(
    session: &mut DocumentSession,
    name: &str,
    arguments: &Value,
) -> Result<Value, GatewayError> {
    if !TOOL_NAMES.contains(&name) {
        return Err(GatewayError::invalid(format!("unknown tool `{name}`")));
    }
    let payload = match name {
        "cad_session_context" => session_context(session, arguments)?,
        "cad_inspect_and_slice" => inspect(session, arguments)?,
        "cad_query_geometry" => query(session, arguments)?,
        "cad_apply_commands" => apply(session, arguments)?,
        "cad_sync_kcl_view" => sync_kcl(session, arguments)?,
        "cad_verify_and_render" => verify(session, arguments)?,
        "cad_branch_control" => branch(session, arguments)?,
        "cad_export_artifacts" => export(session, arguments)?,
        _ => return Err(GatewayError::invalid(format!("unknown tool `{name}`"))),
    };
    Ok(tool_result(payload, false))
}

pub fn tool_error_result(err: &GatewayError) -> Value {
    tool_result(err.to_payload(), true)
}

fn session_context(session: &DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let _parsed: SessionContextArgs = parse_args(args)?;
    let summary = inspect_document_summary(session.graph());
    let context = session.context();
    Ok(json!({
        "mode": context.mode,
        "project_path": session.path().display().to_string(),
        "project_name": context.project_name,
        "revision": context.revision,
        "active_branch": context.active_branch,
        "actor": context.actor,
        "viewport": context.viewport,
        "design_contract": summary.design_contract,
    }))
}

fn inspect(session: &DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: InspectAndSliceArgs = parse_args(args)?;
    let granularity = parsed.granularity.as_deref().unwrap_or("summary");
    let summary = inspect_document_summary(session.graph());
    let labels = untrusted_labels(&summary);
    if granularity == "summary" {
        let report = session.verify(VerificationProfile::Fast);
        return Ok(json!({
            "granularity": "summary",
            "summary": summary,
            "verification_overview": report.layers,
            "untrusted_asset_labels": labels,
        }));
    }
    if granularity != "slice" {
        return Err(GatewayError::invalid(
            "granularity must be summary or slice",
        ));
    }
    let mut focus = parsed.focus_nodes.unwrap_or_default();
    for topo_ref in parsed.focus_topo_refs.unwrap_or_default() {
        if let Some(node) = topo_ref.split("::").next() {
            if !node.is_empty() && !node.contains(':') {
                focus.push(node.to_owned());
            }
        }
    }
    if focus.is_empty() {
        return Err(GatewayError::invalid(
            "slice requires focus_nodes or a creator id in focus_topo_refs",
        ));
    }
    let request = ContextSliceRequest {
        focus_nodes: focus.into_iter().map(NodeId::from).collect(),
        upstream_depth: parsed.upstream_depth.unwrap_or(1),
        downstream_depth: parsed.downstream_depth.unwrap_or(1),
        ensure_valid_closure: true,
    };
    let sliced = slice_document_context(session.graph(), &request)?;
    let sliced_summary = inspect_document_summary(&sliced.graph);
    Ok(json!({
        "granularity": "slice",
        "kept_nodes": sliced.kept_nodes,
        "kept_parameters": sliced.kept_parameters,
        "graph": sliced.graph,
        "summary": sliced_summary,
        "kcl_projection": null,
        "kcl_projection_status": "UNKNOWN",
        "untrusted_asset_labels": labels,
    }))
}

fn query(session: &DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: QueryGeometryArgs = parse_args(args)?;
    match parsed.query.as_str() {
        "explain" => {
            let target = parsed
                .target
                .ok_or_else(|| GatewayError::invalid("explain requires target"))?;
            let explained = cad_api::explain_entity(session.graph(), &target)?;
            Ok(json!({ "query": "explain", "explanation": explained }))
        }
        "topology" => {
            let snapshot = snapshot_from(session, parsed.snapshot);
            let filter = parsed.filter.unwrap_or_default();
            match query_topology(&snapshot, &filter) {
                Ok(result) => Ok(json!({ "query": "topology", "result": result })),
                Err(err) => Err(topo_failure(err, &snapshot)),
            }
        }
        "measure" => {
            if let Some(summary) = finished_solid_measure(session, parsed.kind.as_deref()) {
                return summary;
            }
            let snapshot = snapshot_from(session, parsed.snapshot);
            let left: TopoRef = parsed
                .left
                .ok_or_else(|| GatewayError::invalid("left is required"))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|err| GatewayError::invalid(err.to_string()))
                })
                .map_err(|err| {
                    GatewayError::invalid(format!(
                        "{}. Distance measures require left and right. Use kind volume or bounding_box when you do not have topology refs.",
                        err.message
                    ))
                })?;
            let right: TopoRef = parsed
                .right
                .ok_or_else(|| GatewayError::invalid("right is required"))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|err| GatewayError::invalid(err.to_string()))
                })?;
            let kind: MeasurementKind = match parsed.kind.as_deref() {
                None => MeasurementKind::CentroidDistance,
                Some(name) => parse_distance_kind(name)?,
            };
            let tolerances = parsed.tolerances.unwrap_or_default();
            let options = parsed.options.unwrap_or_default();
            match measure_geometry_refs(&snapshot, &left, &right, kind, &tolerances, &options) {
                Ok(result) => Ok(json!({ "query": "measure", "result": result })),
                Err(err) => Err(measure_failure(err, &snapshot)),
            }
        }
        _ => Err(GatewayError::invalid(
            "query must be topology, measure, or explain",
        )),
    }
}

fn apply(session: &mut DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: ApplyCommandsArgs = parse_args(args)?;
    let expected = parsed.expected_revision;
    let actor: Actor = serde_json::from_value(parsed.actor).map_err(|err| {
        GatewayError::invalid(format!(
            "{err}. actor.kind must be human_ui, builtin_agent, external_agent, python_sdk, or kcl_view. kind=agent is invalid. external_agent requires client and session_id, for example {{\"kind\":\"external_agent\",\"client\":\"grok\",\"session_id\":\"<real session id>\"}}."
        ))
    })?;
    let commands: Vec<cad_api::SemanticCommand> = serde_json::from_value(parsed.commands)
        .map_err(|err| GatewayError::new("E400_INVALID_COMMAND", err.to_string()))?;
    if commands.is_empty() {
        return Err(GatewayError::new(
            "E400_INVALID_COMMAND",
            "commands must contain at least one SemanticCommand",
        ));
    }
    let policy = parsed.branch_policy.as_deref().unwrap_or("preview");
    match policy {
        "active" => exec(session, expected, actor, BranchTarget::Active, commands),
        "main" => {
            reject_live_main(session)?;
            exec(session, expected, actor, BranchTarget::Main, commands)
        }
        "commit_if_pass" => commit_if_pass(session, expected, actor, commands),
        name if name.starts_with("named:") => {
            let branch = name.trim_start_matches("named:").trim();
            if branch.is_empty() {
                return Err(GatewayError::invalid("named branch is empty"));
            }
            exec(
                session,
                expected,
                actor,
                BranchTarget::Named(branch.to_owned()),
                commands,
            )
        }
        "preview" => {
            let branch = preview_branch(&actor);
            ensure_preview(session, &branch, expected)?;
            let outcome = session.exec_semantic_cmds(
                Some(expected),
                actor,
                BranchTarget::Named(branch.clone()),
                commands,
            )?;
            let mut body = json!({
                "revision": outcome.revision,
                "branch": outcome.branch,
                "summary": outcome.summary,
                "created_nodes": outcome.created_nodes,
                "modified_nodes": outcome.modified_nodes,
                "deleted_nodes": outcome.deleted_nodes,
                "solids": outcome.solids,
                "committed": false,
            });
            attach_recovery_hints(&mut body, &outcome.recovery_hints);
            Ok(body)
        }
        _ => Err(GatewayError::invalid(
            "branch_policy must be preview, active, main, commit_if_pass, or named:<branch>",
        )),
    }
}

fn commit_if_pass(
    session: &mut DocumentSession,
    expected: u64,
    actor: Actor,
    commands: Vec<cad_api::SemanticCommand>,
) -> Result<Value, GatewayError> {
    let result = session.exec_semantic_cmds_commit_if_pass(Some(expected), actor, commands)?;
    if result.committed {
        let mut body = json!({
            "committed": true,
            "revision": result.outcome.revision,
            "branch": result.outcome.branch,
            "summary": result.outcome.summary,
            "created_nodes": result.outcome.created_nodes,
            "modified_nodes": result.outcome.modified_nodes,
            "deleted_nodes": result.outcome.deleted_nodes,
            "solids": result.outcome.solids,
            "report": result.report,
        });
        attach_recovery_hints(&mut body, &result.outcome.recovery_hints);
        return Ok(body);
    }
    let mut body = json!({
        "committed": false,
        "revision": result.outcome.revision,
        "branch": result.outcome.branch,
        "summary": result.outcome.summary,
        "report": result.report,
        "message": "Fast L0-L2 is not all PASS, so commit_if_pass discarded the staging preview without modifying main.",
    });
    attach_recovery_hints(&mut body, &result.outcome.recovery_hints);
    Ok(body)
}

fn exec(
    session: &mut DocumentSession,
    expected: u64,
    actor: Actor,
    target: BranchTarget,
    commands: Vec<cad_api::SemanticCommand>,
) -> Result<Value, GatewayError> {
    let outcome = session.exec_semantic_cmds(Some(expected), actor, target, commands)?;
    let mut body = json!({
        "revision": outcome.revision,
        "branch": outcome.branch,
        "summary": outcome.summary,
        "created_nodes": outcome.created_nodes,
        "modified_nodes": outcome.modified_nodes,
        "deleted_nodes": outcome.deleted_nodes,
        "solids": outcome.solids,
    });
    attach_recovery_hints(&mut body, &outcome.recovery_hints);
    Ok(body)
}

fn sync_kcl(session: &mut DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: SyncKclViewArgs = parse_args(args)?;
    match parsed.action.as_str() {
        "emit" => {
            let kcl = session.emit_kcl_view()?;
            Ok(json!({ "action": "emit", "kcl": kcl, "revision": session.revision() }))
        }
        "sync" => {
            let expected = parsed
                .expected_revision
                .ok_or_else(|| GatewayError::invalid("expected_revision is required"))?;
            let actor = parsed
                .actor
                .ok_or_else(|| GatewayError::invalid("actor is required"))?;
            let kcl = parsed
                .kcl
                .as_deref()
                .ok_or_else(|| GatewayError::invalid("sync requires kcl"))?;
            let outcome = session.sync_kcl_view(Some(expected), actor, kcl)?;
            let mut body = json!({
                "action": "sync",
                "revision": outcome.revision,
                "branch": outcome.branch,
                "summary": outcome.summary,
                "solids": outcome.solids,
            });
            attach_recovery_hints(&mut body, &outcome.recovery_hints);
            Ok(body)
        }
        _ => Err(GatewayError::invalid("action must be emit or sync")),
    }
}

fn verify(session: &DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: VerifyAndRenderArgs = parse_args(args)?;
    let profile = parse_profile(parsed.profile.as_deref())?;
    let report = session.verify(profile);
    let render = parsed.render.unwrap_or(false);
    let views = if render {
        let requested = parsed.views.unwrap_or_default();
        let views = if requested.is_empty() {
            vec!["iso".into(), "front".into(), "top".into(), "right".into()]
        } else {
            requested
        };
        for view in &views {
            if !matches!(view.as_str(), "iso" | "front" | "top" | "right") {
                return Err(GatewayError::invalid(
                    "views must be iso, front, top, or right",
                ));
            }
        }
        json!({
            "requested": true,
            "status": "UNKNOWN",
            "views": views,
            "images": [],
            "note": "The gateway does not rasterize. Pixels are not a PASS verdict.",
        })
    } else {
        json!({
            "requested": false,
            "status": "UNKNOWN",
            "views": [],
            "images": [],
        })
    };
    Ok(json!({
        "profile": profile.as_str(),
        "report": report,
        "solids": session.solids(),
        "render": views,
    }))
}

fn branch(session: &mut DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: BranchControlArgs = parse_args(args)?;
    let require_branch_name = |branch: Option<String>| -> Result<String, GatewayError> {
        branch
            .filter(|value| !value.is_empty())
            .ok_or_else(|| GatewayError::invalid("branch is required"))
    };
    match parsed.action.as_str() {
        "list" => Ok(json!({ "action": "list", "branches": session.list_branches()? })),
        "diff" => {
            let name = require_branch_name(parsed.branch)?;
            Ok(json!({ "action": "diff", "diff": session.diff_branch(&name)? }))
        }
        "request_review" => {
            let name = match parsed.branch {
                Some(name) if !name.is_empty() => name,
                _ => session.active_branch().to_owned(),
            };
            let diff = session.diff_branch(&name)?;
            Ok(json!({
                "action": "request_review",
                "branch": name,
                "diff": diff,
                "desktop_delivery": "UNKNOWN",
                "message": "No desktop Change Preview host is attached. Use the diff as the review payload.",
            }))
        }
        "commit_preview" => {
            let name = require_branch_name(parsed.branch)?;
            let outcome = session.commit_preview(&name)?;
            let mut body = json!({
                "action": "commit_preview",
                "revision": outcome.revision,
                "branch": outcome.branch,
                "summary": outcome.summary,
                "solids": outcome.solids,
            });
            attach_recovery_hints(&mut body, &outcome.recovery_hints);
            Ok(body)
        }
        "discard_preview" => {
            let name = require_branch_name(parsed.branch)?;
            session.discard_preview(&name)?;
            Ok(json!({ "action": "discard_preview", "branch": name, "revision": session.revision() }))
        }
        "undo" => {
            let outcome = session.undo()?;
            Ok(json!({ "action": "undo", "outcome": outcome }))
        }
        "redo" => {
            let outcome = session.redo()?;
            Ok(json!({ "action": "redo", "outcome": outcome }))
        }
        _ => Err(GatewayError::invalid(
            "action must be list, diff, request_review, commit_preview, discard_preview, undo, or redo",
        )),
    }
}

fn export(session: &DocumentSession, args: &Value) -> Result<Value, GatewayError> {
    let parsed: ExportArtifactsArgs = parse_args(args)?;
    if parsed.format.is_empty() {
        return Err(GatewayError::invalid("format is required"));
    }
    if parsed.path.is_empty() {
        return Err(GatewayError::invalid("path is required"));
    }
    let label = parsed
        .part_label
        .as_deref()
        .map(sanitize_untrusted_label);
    let node_id = parsed
        .node_id
        .as_deref()
        .map(sanitize_untrusted_label);
    let report = session.export_artifact_selection(
        &parsed.format,
        Path::new(&parsed.path),
        label.as_deref(),
        node_id.as_deref(),
    )?;
    let mut body = serde_json::to_value(&report)
        .map_err(|err| GatewayError::invalid(err.to_string()))?;
    body["untrusted_asset_labels"] = json!(label.into_iter().collect::<Vec<_>>());
    Ok(body)
}

fn ensure_preview(
    session: &mut DocumentSession,
    branch: &str,
    expected: u64,
) -> Result<(), GatewayError> {
    if branch_revision(session, branch)?.is_some() {
        return Ok(());
    }
    let main = branch_revision(session, "main")?.unwrap_or(session.revision());
    if expected != main {
        return Err(GatewayError::new(
            "E409_REVISION_CONFLICT",
            format!("E409_REVISION_CONFLICT: expected revision {expected}, actual {main}"),
        ));
    }
    match session.branch_preview(branch) {
        Ok(_) => Ok(()),
        Err(err) if err.code() == "E_BRANCH_EXISTS" => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn preview_branch(actor: &Actor) -> String {
    let (client, session_id) = match actor {
        Actor::ExternalAgent { client, session_id } => (client.as_str(), session_id.as_str()),
        _ => ("mcp", "default"),
    };
    format!(
        "agent/{}/{}",
        branch_component(client),
        branch_component(session_id)
    )
}

fn branch_component(raw: &str) -> String {
    let cleaned = sanitize_untrusted_label(raw);
    let mut out = String::new();
    for ch in cleaned.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "mcp".to_owned()
    } else {
        trimmed
    }
}

fn branch_revision(session: &DocumentSession, name: &str) -> Result<Option<u64>, GatewayError> {
    if session.active_branch() == name {
        return Ok(Some(session.revision()));
    }
    let branches = session.list_branches()?;
    Ok(branches.into_iter().find(|info| info.branch_name == name).map(|info| {
        u64::try_from(info.head_seq).unwrap_or(0)
    }))
}

fn reject_live_main(session: &DocumentSession) -> Result<(), GatewayError> {
    if session.mode() == RuntimeMode::LiveAttached {
        Err(GatewayError::invalid(
            "a live_attached session does not write main from this gateway",
        ))
    } else {
        Ok(())
    }
}

fn measure_failure(
    err: cad_topo::MeasurementError,
    snapshot: &TopoSnapshot,
) -> GatewayError {
    let base = GatewayError::from(err.clone());
    let cad_topo::MeasurementError::AmbiguousReference { entity_ids, .. } = &err else {
        return base;
    };
    let matched = matching_candidates(snapshot, entity_ids);
    let hint = build_ambiguous_topo_hints(&matched, 8);
    let hint_text = serde_json::to_string(&hint).unwrap_or_default();
    let hint_value = serde_json::to_value(&hint).ok();
    GatewayError::new(
        base.code,
        format!("{} recovery_hints={hint_text}", base.message),
    )
    .with_recovery_hints(hint_value.into_iter().collect())
}

fn topo_failure(err: TopoError, snapshot: &TopoSnapshot) -> GatewayError {
    if let TopoError::AmbiguousMatch { candidates, .. } = &err {
        let matched = matching_candidates(snapshot, candidates);
        let hint = build_ambiguous_topo_hints(&matched, 8);
        let hint_text = serde_json::to_string(&hint).unwrap_or_default();
        let hint_value = serde_json::to_value(&hint).ok();
        return GatewayError::new(
            "E201_AMBIGUOUS_TOPOLOGY",
            format!("{err}. recovery_hints={hint_text}"),
        )
        .with_recovery_hints(hint_value.into_iter().collect());
    }
    err.into()
}

fn matching_candidates(snapshot: &TopoSnapshot, ids: &[TopoEntityId]) -> Vec<CandidateEntity> {
    snapshot
        .candidates
        .iter()
        .filter(|candidate| ids.iter().any(|id| id == &candidate.entity_id))
        .cloned()
        .collect()
}

fn snapshot_from(session: &DocumentSession, wires: Option<Vec<WireCandidate>>) -> TopoSnapshot {
    let Some(wires) = wires else {
        return session.topo_snapshot().clone();
    };
    let candidates = wires
        .into_iter()
        .map(|wire| CandidateEntity {
            entity_id: TopoEntityId(wire.entity_id),
            creator_node_id: wire.creator_node_id,
            local_output_slot: wire.local_output_slot,
            evolution_chain: wire.evolution_chain,
            fingerprint: wire.fingerprint,
            provenance_tags: wire.provenance_tags,
        })
        .collect();
    TopoSnapshot::new(candidates)
}

#[derive(Debug, Deserialize)]
struct WireCandidate {
    entity_id: String,
    creator_node_id: String,
    local_output_slot: String,
    #[serde(default)]
    evolution_chain: Vec<EvolutionRecord>,
    fingerprint: GeometricFingerprint,
    #[serde(default)]
    provenance_tags: Vec<String>,
}

fn untrusted_labels(summary: &cad_api::DocumentSummary) -> Vec<String> {
    summary
        .feature_outline
        .iter()
        .filter(|entry| entry.feature == "import_step")
        .map(|entry| sanitize_untrusted_label(&entry.name))
        .collect()
}

fn finished_solid_measure(
    session: &DocumentSession,
    kind: Option<&str>,
) -> Option<Result<Value, GatewayError>> {
    let kind = kind?;
    if !matches!(kind, "volume" | "Volume" | "bounding_box" | "BoundingBox") {
        return None;
    }
    let solids = session.solids();
    let canonical = if kind.eq_ignore_ascii_case("volume") {
        "volume"
    } else {
        "bounding_box"
    };
    Some(Ok(json!({
        "query": "measure",
        "result": {
            "kind": canonical,
            "volume_mm3": solids.total_volume_mm3,
            "bbox_min_mm": solids.bbox_min,
            "bbox_max_mm": solids.bbox_max,
            "unit": "mm",
        }
    })))
}

fn parse_distance_kind(name: &str) -> Result<MeasurementKind, GatewayError> {
    serde_json::from_value(Value::String(name.to_owned())).map_err(|_| {
        GatewayError::invalid(format!(
            "unknown measure kind `{name}`. Use volume, bounding_box, CentroidDistance, ParallelPlaneDistance, Coaxiality, or AngleDeg."
        ))
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

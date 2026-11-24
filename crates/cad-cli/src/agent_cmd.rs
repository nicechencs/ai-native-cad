//! Read-only session, query, and verify commands, plus `doctor-agent`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cad_api::{ExplainTarget, NodeId, SemanticErrorCode, explain_entity, inspect_document_summary};
use cad_runtime::{RUNTIME_DISCOVERY_FILE, discovery_pid_is_stale, read_runtime_discovery};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime_host::{self, HostError};

const TOOLS_LIST_TOKEN_LIMIT: usize = 3_500;

const NO_PROJECT_NO_DESKTOP_HINT: &str = "Configured cad-cli mcp has no --project, and this workspace has no active .aicad-runtime.json desktop session. Add `--project <path.aicad>` for headless or CI mode, or start the desktop app for Live-Attach.";

#[derive(Args)]
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Print mode, project, revision, branch, viewport, and design contract.
    Status {
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
        #[arg(long, value_name = "DIR", default_value = ".")]
        workspace: PathBuf,
        /// Stay in this process even when a desktop Live-Attach host is running.
        #[arg(long)]
        headless: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
pub(crate) struct QueryArgs {
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    #[arg(long, value_name = "DIR", default_value = ".")]
    workspace: PathBuf,
    /// Stay in this process even when a desktop Live-Attach host is running.
    #[arg(long, global = true)]
    headless: bool,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: QueryCommand,
}

#[derive(Subcommand)]
enum QueryCommand {
    /// Explain a feature node or a parameter by id.
    Explain {
        #[arg(long, value_name = "ID")]
        target: String,
    },
    /// Print the parameter table, feature outline, and design contract.
    Summary,
}

#[derive(Args)]
pub(crate) struct VerifyArgs {
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    #[arg(long, value_name = "DIR", default_value = ".")]
    workspace: PathBuf,
    /// Verification profile: `fast` or `release`.
    #[arg(long, value_name = "PROFILE")]
    profile: Option<String>,
    /// Stay in this process even when a desktop Live-Attach host is running.
    #[arg(long)]
    headless: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub(crate) struct DoctorArgs {
    #[arg(long, value_name = "DIR", default_value = ".")]
    workspace: PathBuf,
    /// Optional external agent client preflight check: claude, codex, or grok.
    #[arg(long, value_name = "CLIENT")]
    client: Option<String>,
    #[arg(long)]
    json: bool,
}

pub(crate) fn run_session(args: SessionArgs) -> ExitCode {
    let SessionCommand::Status {
        project,
        workspace,
        headless,
        json,
    } = args.command;
    let result = if let Some(live) = runtime_host::try_live_client(&workspace, headless) {
        live.call_tool("cad_session_context", json!({}))
    } else {
        let project = match runtime_host::require_project(project.as_deref()) {
            Ok(project) => project,
            Err(err) => return fail(json, &err),
        };
        match runtime_host::open_existing(&workspace, project) {
            Ok(session) => runtime_host::mcp_tool_call(session, "cad_session_context", json!({})),
            Err(err) => Err(err),
        }
    };
    match result {
        Ok(payload) => finish(json, &payload, || print_session_text(&payload)),
        Err(err) => fail(json, &err),
    }
}

pub(crate) fn run_query(args: QueryArgs) -> ExitCode {
    let json = args.json;
    if let Some(live) = runtime_host::try_live_client(&args.workspace, args.headless) {
        let payload = match args.command {
            QueryCommand::Summary => query_summary_live(&live),
            QueryCommand::Explain { target } => query_explain_live(&live, &target),
        };
        return match payload {
            Ok(payload) => finish(json, &payload, || print_query_text(&payload)),
            Err(err) => fail(json, &err),
        };
    }
    let project = match runtime_host::require_project(args.project.as_deref()) {
        Ok(project) => project,
        Err(err) => return fail(json, &err),
    };
    let session = match runtime_host::open_existing(&args.workspace, project) {
        Ok(session) => session,
        Err(err) => return fail(json, &err),
    };
    let payload = match args.command {
        QueryCommand::Summary => json!({
            "query": "summary",
            "project_path": session.path().display().to_string(),
            "revision": session.revision(),
            "active_branch": session.active_branch(),
            "summary": inspect_document_summary(session.graph()),
        }),
        QueryCommand::Explain { target } => match explain(&session, &target) {
            Ok(payload) => payload,
            Err(err) => return fail(json, &err),
        },
    };
    finish(json, &payload, || print_query_text(&payload))
}

pub(crate) fn run_verify(args: VerifyArgs) -> ExitCode {
    let tool_args = match args
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(profile) => json!({ "profile": profile.to_ascii_lowercase() }),
        None => json!({}),
    };
    let result = if let Some(live) = runtime_host::try_live_client(&args.workspace, args.headless) {
        live.call_tool("cad_verify_and_render", tool_args)
    } else {
        let project = match runtime_host::require_project(args.project.as_deref()) {
            Ok(project) => project,
            Err(err) => return fail(args.json, &err),
        };
        match runtime_host::open_existing(&args.workspace, project) {
            Ok(session) => runtime_host::mcp_tool_call(session, "cad_verify_and_render", tool_args),
            Err(err) => Err(err),
        }
    };
    match result {
        Ok(payload) => {
            let exit = finish(args.json, &payload, || print_verify_text(&payload));
            let failed = payload["report"]["overall_status"]
                .as_str()
                .is_some_and(|status| status.eq_ignore_ascii_case("FAIL"));
            if failed {
                ExitCode::from(1)
            } else {
                exit
            }
        }
        Err(err) => fail(args.json, &err),
    }
}

fn query_summary_live(live: &runtime_host::LiveClient) -> Result<Value, HostError> {
    let ctx = live.call_tool("cad_session_context", json!({}))?;
    let inspect = live.call_tool("cad_inspect_and_slice", json!({ "granularity": "summary" }))?;
    Ok(json!({
        "query": "summary",
        "project_path": ctx["project_path"],
        "revision": ctx["revision"],
        "active_branch": ctx["active_branch"],
        "summary": inspect["summary"],
    }))
}

fn query_explain_live(live: &runtime_host::LiveClient, target: &str) -> Result<Value, HostError> {
    match live.call_tool(
        "cad_query_geometry",
        json!({
            "query": "explain",
            "target": { "target": "node", "node_id": target },
        }),
    ) {
        Ok(payload) => Ok(json!({
            "query": "explain",
            "target": target,
            "explanation": payload["explanation"],
        })),
        Err(err) if err.code == "E404_NODE_NOT_FOUND" => match live.call_tool(
            "cad_query_geometry",
            json!({
                "query": "explain",
                "target": { "target": "parameter", "name": target },
            }),
        ) {
            Ok(payload) => Ok(json!({
                "query": "explain",
                "target": target,
                "explanation": payload["explanation"],
            })),
            Err(_) => Err(HostError::new(
                "E404_NODE_NOT_FOUND",
                format!("`{target}` is not a feature node or a parameter"),
            )),
        },
        Err(err) => Err(err),
    }
}

pub(crate) fn run_doctor(args: DoctorArgs) -> ExitCode {
    let report = doctor_report(&args.workspace, args.client.as_deref());
    let ok = report["status"] == "ok";
    let printed = finish(args.json, &report, || print_doctor_text(&report));
    if ok { printed } else { ExitCode::from(1) }
}

fn explain(session: &cad_runtime::DocumentSession, target: &str) -> Result<Value, HostError> {
    let graph = session.graph();
    let node = ExplainTarget::Node {
        node_id: NodeId::from(target),
    };
    match explain_entity(graph, &node) {
        Ok(explanation) => Ok(json!({
            "query": "explain",
            "target": target,
            "explanation": explanation,
        })),
        Err(err) if err.code == SemanticErrorCode::E404NodeNotFound => {
            let parameter = ExplainTarget::Parameter {
                name: target.to_owned(),
            };
            match explain_entity(graph, &parameter) {
                Ok(explanation) => Ok(json!({
                    "query": "explain",
                    "target": target,
                    "explanation": explanation,
                })),
                Err(_) => Err(HostError::new(
                    "E404_NODE_NOT_FOUND",
                    format!("`{target}` is not a feature node or a parameter"),
                )),
            }
        }
        Err(err) => Err(HostError::new(err.code.as_str(), err.to_string())),
    }
}

fn doctor_report(workspace: &Path, client: Option<&str>) -> Value {
    let binary = std::env::current_exe();
    let binary_path = binary
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let binary_ok = binary.as_ref().is_ok_and(|path| path.is_file());
    let tokens = cad_mcp::token_budget_of_tools_list();
    let budget_ok = tokens <= TOOLS_LIST_TOKEN_LIMIT;
    let workspace_path = runtime_host::display_path(workspace);
    let writable = workspace_writable(&workspace_path);
    let live = live_attach(&workspace_path);
    let live_ok = live["ok"].as_bool().unwrap_or(false);
    let kernel = cad_kernel_occt::default_brep_kernel();
    let backend = kernel.backend_name().to_owned();
    let authoritative = backend.to_ascii_lowercase().contains("occt");
    let client_check = client.map(|name| client_preflight(name, &workspace_path));
    let client_ok = client_check
        .as_ref()
        .is_none_or(|value| value["ok"].as_bool().unwrap_or(false));
    let status = if binary_ok && budget_ok && writable && live_ok && client_ok {
        "ok"
    } else {
        "error"
    };
    let mut report = json!({
        "status": status,
        "binary": {
            "ok": binary_ok,
            "path": binary_path,
        },
        "kernel": {
            "backend": backend,
            "authoritative": authoritative,
        },
        "token_budget": {
            "ok": budget_ok,
            "tokens": tokens,
            "limit": TOOLS_LIST_TOKEN_LIMIT,
        },
        "workspace": {
            "ok": writable,
            "writable": writable,
            "path": workspace_path.display().to_string(),
        },
        "live_attach": live,
    });
    if let Some(check) = client_check {
        report["client_preflight"] = check;
    }
    report
}

fn client_preflight(client: &str, workspace: &Path) -> Value {
    let normalized = client.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "claude" | "claude-code" => {
            let mcp_json = workspace.join(".mcp.json");
            let text = std::fs::read_to_string(&mcp_json).unwrap_or_default();
            let configured = text.contains("mcpServers") && text.contains("aicad");
            let gap = if configured {
                headless_gap_hint(&cad_cli_mcp_args_from_json(&text), workspace)
            } else {
                None
            };
            json!({
                "client": "claude",
                "ok": configured && gap.is_none(),
                "config_path": mcp_json.display().to_string(),
                "hint": gap.unwrap_or(if configured {
                    "Run claude -p ... --mcp-config .mcp.json --strict-mcp-config"
                } else {
                    "Missing aicad entry in .mcp.json. Copy skills/aicad-engineering/templates/claude-code.mcp.json."
                }),
            })
        }
        "codex" => {
            let ws_cfg = workspace.join(".codex").join("config.toml");
            let home_cfg = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".codex").join("config.toml"));
            let found = [Some(ws_cfg.clone()), home_cfg]
                .into_iter()
                .flatten()
                .find(|path| {
                    std::fs::read_to_string(path)
                        .ok()
                        .is_some_and(|text| text.contains("aicad"))
                });
            let text = found
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok());
            let gap = text
                .as_deref()
                .and_then(|body| headless_gap_hint(&cad_cli_mcp_args_from_toml(body), workspace));
            json!({
                "client": "codex",
                "ok": found.is_some() && gap.is_none(),
                "config_path": found.unwrap_or(ws_cfg).display().to_string(),
                "hint": gap.unwrap_or(
                    "Copy skills/aicad-engineering/templates/codex-config.toml into .codex/config.toml.",
                ),
            })
        }
        "grok" => {
            let has_token = std::env::var("AICAD_MCP_TOKEN")
                .ok()
                .is_some_and(|value| !value.trim().is_empty());
            let home_mcp = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".grok").join("mcp.json"));
            let grok_text = home_mcp
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok());
            let has_grok_mcp = grok_text
                .as_deref()
                .is_some_and(|text| text.contains("aicad"));
            let gap = grok_text.as_deref().and_then(|body| {
                if body.contains("aicad") {
                    headless_gap_hint(&cad_cli_mcp_args_from_json(body), workspace)
                } else {
                    None
                }
            });
            json!({
                "client": "grok",
                "ok": (has_token || has_grok_mcp) && gap.is_none(),
                "hint": gap.unwrap_or(
                    "Grok CLI does not auto-load .mcp.json. Register via `grok mcp` or loopback HTTP with AICAD_MCP_TOKEN (see skills/aicad-engineering/templates/grok-http-setup.md).",
                ),
            })
        }
        other => json!({
            "client": other,
            "ok": false,
            "hint": "Supported --client values: claude, codex, grok.",
        }),
    }
}

fn headless_gap_hint(arg_lists: &[Vec<String>], workspace: &Path) -> Option<&'static str> {
    if desktop_session_active(workspace) {
        return None;
    }
    if arg_lists.iter().any(|args| args_include_project(args)) {
        return None;
    }
    // An empty list means the aicad server was not a readable cad-cli mcp
    // launch. That is a gap, not a healthy headless config.
    Some(NO_PROJECT_NO_DESKTOP_HINT)
}

fn desktop_session_active(workspace: &Path) -> bool {
    read_runtime_discovery(workspace)
        .ok()
        .is_some_and(|info| !discovery_pid_is_stale(&info))
}

fn args_include_project(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--project" {
            let value = args.get(index + 1).map(String::as_str).unwrap_or("").trim();
            if !value.is_empty() && !value.starts_with('-') {
                return true;
            }
        } else if let Some(value) = arg.strip_prefix("--project=") {
            if !value.trim().is_empty() {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn command_is_cad_cli(command: &str) -> bool {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    let lower = name.to_ascii_lowercase();
    if lower == "run-cad-cli-occt.ps1" || lower == "run-cad-cli-occt.sh" {
        return true;
    }
    let stem = name
        .rsplit_once('.')
        .map(|(stem, ext)| {
            if ext.eq_ignore_ascii_case("exe")
                || ext.eq_ignore_ascii_case("cmd")
                || ext.eq_ignore_ascii_case("bat")
            {
                stem
            } else {
                name
            }
        })
        .unwrap_or(name);
    stem.eq_ignore_ascii_case("cad-cli")
}

fn args_launch_cad_cli(args: &[String]) -> bool {
    args.iter().any(|arg| {
        let name = Path::new(arg)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(arg.as_str());
        command_is_cad_cli(name)
    })
}

fn cad_cli_mcp_args_from_json(text: &str) -> Vec<Vec<String>> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(server) = value.get("mcpServers").and_then(|entry| entry.get("aicad")) else {
        return Vec::new();
    };
    mcp_stdio_args(server).into_iter().collect()
}

fn mcp_stdio_args(server: &Value) -> Option<Vec<String>> {
    let command = server.get("command")?.as_str()?;
    let args = server
        .get("args")?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    if !command_is_cad_cli(command) && !args_launch_cad_cli(&args) {
        return None;
    }
    if args.iter().any(|arg| arg == "mcp") {
        Some(args)
    } else {
        None
    }
}

fn cad_cli_mcp_args_from_toml(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut header = String::new();
    let mut body = String::new();
    for line in text.lines() {
        let code = strip_toml_comment(line);
        let trimmed = code.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.starts_with("[[") {
            push_toml_aicad_args(&header, &body, &mut out);
            header = trimmed.to_owned();
            body.clear();
            continue;
        }
        body.push_str(code);
        body.push('\n');
    }
    push_toml_aicad_args(&header, &body, &mut out);
    out
}

fn push_toml_aicad_args(header: &str, body: &str, out: &mut Vec<Vec<String>>) {
    if !header.to_ascii_lowercase().contains("aicad") {
        return;
    }
    let Some(command) = toml_assignment_string(body, "command") else {
        return;
    };
    let Some(args) = toml_assignment_string_array(body, "args") else {
        return;
    };
    if !command_is_cad_cli(&command) && !args_launch_cad_cli(&args) {
        return;
    }
    if args.iter().any(|arg| arg == "mcp") {
        out.push(args);
    }
}

fn toml_assignment_string(code: &str, key: &str) -> Option<String> {
    for line in code.lines() {
        if let Some(value) = toml_key_value(line, key) {
            return parse_toml_string(value.trim());
        }
    }
    None
}

fn toml_assignment_string_array(code: &str, key: &str) -> Option<Vec<String>> {
    let mut offset = 0usize;
    for line in code.split_inclusive('\n') {
        if let Some(value_at) = toml_key_value_index(line, key) {
            return take_balanced_array(&code[offset + value_at..])
                .and_then(parse_toml_string_array);
        }
        offset += line.len();
    }
    None
}

fn toml_key_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    toml_key_value_index(line, key).map(|index| &line[index..])
}

fn toml_key_value_index(line: &str, key: &str) -> Option<usize> {
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    let rest = trimmed.strip_prefix(key)?;
    if !rest.starts_with(|ch: char| ch == '=' || ch.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('=')?;
    Some(trimmed_start + (trimmed.len() - rest.len()))
}

fn strip_toml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if escape {
                escape = false;
            } else if byte == b'\\' && active == b'"' {
                escape = true;
            } else if byte == active {
                quote = None;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == b'#' {
            return &line[..index];
        }
        index += 1;
    }
    line
}

fn take_balanced_array(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let bytes = text.as_bytes();
    let mut index = start;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if escape {
                escape = false;
            } else if byte == b'\\' && active == b'"' {
                escape = true;
            } else if byte == active {
                quote = None;
            }
        } else {
            match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[start..=index]);
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

fn parse_toml_string_array(array_src: &str) -> Option<Vec<String>> {
    let inner = array_src.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    let mut rest = inner.trim();
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix(',') {
            rest = next.trim_start();
            continue;
        }
        let (value, next) = parse_toml_string_span(rest)?;
        out.push(value);
        rest = next.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next.trim_start();
        }
    }
    Some(out)
}

fn parse_toml_string(input: &str) -> Option<String> {
    parse_toml_string_span(input).map(|(value, _)| value)
}

fn parse_toml_string_span(input: &str) -> Option<(String, &str)> {
    let mut chars = input.char_indices();
    let (_, quote) = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut out = String::new();
    while let Some((index, ch)) = chars.next() {
        if ch == quote {
            let next = input.get(index + quote.len_utf8()..).unwrap_or("");
            return Some((out, next));
        }
        if ch == '\\' && quote == '"' {
            let (_, escaped) = chars.next()?;
            out.push(match escaped {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                '"' => '"',
                other => other,
            });
            continue;
        }
        out.push(ch);
    }
    None
}

fn workspace_writable(workspace: &Path) -> bool {
    if !workspace.is_dir() {
        return false;
    }
    let probe = workspace.join(".aicad-doctor-write-probe");
    let wrote = std::fs::write(&probe, b"ok").is_ok();
    if wrote {
        let _ = std::fs::remove_file(&probe);
    }
    wrote
}

fn live_attach(workspace: &Path) -> Value {
    let path = workspace.join(RUNTIME_DISCOVERY_FILE);
    if !path.is_file() {
        return json!({
            "ok": true,
            "present": false,
            "discovery_file": RUNTIME_DISCOVERY_FILE,
            "status": "absent",
            "pid_alive": false,
            "message": "No desktop session is published. Headless mode is available.",
        });
    }
    match read_runtime_discovery(workspace) {
        Ok(info) => {
            let stale = discovery_pid_is_stale(&info);
            json!({
                "ok": true,
                "present": true,
                "discovery_file": RUNTIME_DISCOVERY_FILE,
                "status": if stale { "stale" } else { "live" },
                "pid_alive": !stale,
                "stale": stale,
                "pid": info.pid,
                "ipc_endpoint": info.ipc_endpoint,
                "active_project": info.active_project,
                "message": if stale {
                    "The discovery file is stale. Headless mode is available."
                } else {
                    "A desktop session is alive and can be live-attached."
                },
            })
        }
        Err(err) => json!({
            "ok": false,
            "present": true,
            "discovery_file": RUNTIME_DISCOVERY_FILE,
            "status": "unreadable",
            "pid_alive": false,
            "message": err.to_string(),
        }),
    }
}

fn finish(json: bool, payload: &Value, text: impl FnOnce()) -> ExitCode {
    if json {
        print_value(payload)
    } else {
        text();
        ExitCode::SUCCESS
    }
}

fn print_session_text(payload: &Value) {
    println!(
        "{}  {}  revision {}  branch {}",
        payload["mode"].as_str().unwrap_or("unknown"),
        payload["project_name"].as_str().unwrap_or(""),
        payload["revision"].as_u64().unwrap_or(0),
        payload["active_branch"].as_str().unwrap_or("")
    );
    println!(
        "project: {}",
        payload["project_path"].as_str().unwrap_or("")
    );
    let viewport = &payload["viewport"];
    let topo = viewport["selected_topo_refs"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let nodes = viewport["selected_node_ids"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    println!("viewport: {topo} topo refs, {nodes} nodes");
    match payload["design_contract"]
        .get("part_name")
        .and_then(Value::as_str)
    {
        Some(name) => println!("design contract: {name}"),
        None => println!("design contract: none"),
    }
}

fn print_query_text(payload: &Value) {
    match payload["query"].as_str() {
        Some("summary") => {
            let summary = &payload["summary"];
            let params = summary["parameters"].as_array().map(Vec::len).unwrap_or(0);
            let features = summary["feature_outline"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            println!(
                "summary revision {} on {}: {params} parameters, {features} features",
                payload["revision"].as_u64().unwrap_or(0),
                payload["active_branch"].as_str().unwrap_or("")
            );
        }
        Some("explain") => {
            let explanation = &payload["explanation"];
            println!(
                "explain {} ({})",
                explanation["id"].as_str().unwrap_or(""),
                explanation["display_name"].as_str().unwrap_or("")
            );
        }
        _ => println!("{payload}"),
    }
}

fn print_verify_text(payload: &Value) {
    let report = &payload["report"];
    println!(
        "overall: {}",
        report["overall_status"].as_str().unwrap_or("UNKNOWN")
    );
    if let Some(layers) = report["layers"].as_object() {
        for (name, status) in layers {
            println!("{name}: {}", status.as_str().unwrap_or("UNKNOWN"));
        }
    }
}

fn print_doctor_text(report: &Value) {
    println!(
        "doctor-agent: {}",
        report["status"].as_str().unwrap_or("error")
    );
    println!(
        "binary: {} ({})",
        if report["binary"]["ok"].as_bool().unwrap_or(false) {
            "ok"
        } else {
            "missing"
        },
        report["binary"]["path"].as_str().unwrap_or("")
    );
    println!(
        "mcp tools/list: {} tokens (limit {})",
        report["token_budget"]["tokens"].as_u64().unwrap_or(0),
        report["token_budget"]["limit"].as_u64().unwrap_or(0)
    );
    println!(
        "workspace: {} {}",
        if report["workspace"]["writable"].as_bool().unwrap_or(false) {
            "writable"
        } else {
            "not writable"
        },
        report["workspace"]["path"].as_str().unwrap_or("")
    );
    println!(
        "live attach: {}",
        report["live_attach"]["status"].as_str().unwrap_or("absent")
    );
    if let Some(client) = report.get("client_preflight") {
        println!(
            "client {}: {}",
            client["client"].as_str().unwrap_or(""),
            if client["ok"].as_bool().unwrap_or(false) {
                "ok"
            } else {
                "check"
            }
        );
        if let Some(hint) = client["hint"].as_str() {
            println!("hint: {hint}");
        }
    }
}

fn print_value(value: &impl Serialize) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(
            true,
            &HostError::new("E_IO", format!("cannot encode JSON: {err}")),
        ),
    }
}

fn fail(json: bool, err: &HostError) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "error",
                "code": err.code,
                "message": err.message,
            })
        );
    }
    eprintln!("error: {}: {}", err.code, err.message);
    ExitCode::from(1)
}

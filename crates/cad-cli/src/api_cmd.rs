//! `cad-cli api apply`: one atomic batch of semantic commands.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use cad_api::SemanticCommand;
use cad_runtime::{Actor, BranchTarget, DocumentSession, ExecOutcome};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime_host::{self, HostError, LiveClient};

#[derive(Args)]
pub(crate) struct ApiArgs {
    #[command(subcommand)]
    command: ApiCommand,
}

#[derive(Subcommand)]
enum ApiCommand {
    /// Apply a batch of semantic commands and print the new revision.
    Apply {
        /// `.aicad` project to open or create.
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
        /// Workspace root. Relative project paths stay inside this directory.
        #[arg(long, value_name = "DIR", default_value = ".")]
        workspace: PathBuf,
        /// JSON array, one command object, or `@path` to a JSON file.
        #[arg(long, value_name = "JSON_OR_@FILE")]
        commands: Option<String>,
        /// Reject the batch when the target branch is no longer at this revision.
        #[arg(long, value_name = "REV")]
        expected_revision: Option<u64>,
        /// Branch to write. A missing name is created from `main`.
        #[arg(long, value_name = "NAME")]
        branch: Option<String>,
        /// Stay in this process even when a desktop Live-Attach host is running.
        #[arg(long)]
        headless: bool,
        /// Print the outcome as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn run(args: ApiArgs) -> ExitCode {
    match args.command {
        ApiCommand::Apply {
            project,
            workspace,
            commands,
            expected_revision,
            branch,
            headless,
            json,
        } => apply(
            project,
            workspace,
            commands,
            expected_revision,
            branch,
            headless,
            json,
        ),
    }
}

fn apply(
    project: Option<PathBuf>,
    workspace: PathBuf,
    commands: Option<String>,
    expected_revision: Option<u64>,
    branch: Option<String>,
    headless: bool,
    json: bool,
) -> ExitCode {
    let Some(commands) = commands else {
        return fail(
            json,
            &HostError::new(
                "E400_INVALID_COMMAND",
                "api apply requires --commands <json-or-@file>",
            ),
        );
    };
    let commands = match load_commands(&commands) {
        Ok(commands) => commands,
        Err(err) => return fail(json, &err),
    };
    if let Some(live) = runtime_host::try_live_client(&workspace, headless) {
        return match apply_live(&live, commands, expected_revision, branch.as_deref()) {
            Ok(outcome) => print_outcome(&outcome, json),
            Err(err) => fail(json, &err),
        };
    }
    let project = match runtime_host::require_project(project.as_deref()) {
        Ok(project) => project,
        Err(err) => return fail(json, &err),
    };
    let mut session = match runtime_host::open_or_create(&workspace, project) {
        Ok(session) => session,
        Err(err) => return fail(json, &err),
    };
    match apply_batch(&mut session, commands, expected_revision, branch.as_deref()) {
        Ok(outcome) => print_outcome(&outcome, json),
        Err(err) => fail(json, &HostError::from_runtime(err)),
    }
}

fn apply_live(
    live: &LiveClient,
    commands: Vec<SemanticCommand>,
    expected_revision: Option<u64>,
    branch: Option<&str>,
) -> Result<ExecOutcome, HostError> {
    const LIVE_DEFAULT_PREVIEW_BRANCH: &str = "agent/cad-cli/api-apply";
    let trimmed_branch = branch.map(str::trim).filter(|name| !name.is_empty());
    let listed_branches = || -> Result<Vec<serde_json::Value>, HostError> {
        let listed = live.call_tool("cad_branch_control", json!({ "action": "list" }))?;
        Ok(listed["branches"].as_array().cloned().unwrap_or_default())
    };
    let head_of = |branches: &[serde_json::Value], name: &str| -> Option<u64> {
        branches
            .iter()
            .find(|item| item["branch_name"].as_str() == Some(name))
            .and_then(|item| item["head_seq"].as_u64())
    };
    let expected = match expected_revision {
        Some(rev) => rev,
        None => {
            let branches = listed_branches()?;
            let target_lookup = trimmed_branch.unwrap_or(LIVE_DEFAULT_PREVIEW_BRANCH);
            if let Some(head) = head_of(&branches, target_lookup) {
                head
            } else {
                let ctx = live.call_tool("cad_session_context", json!({}))?;
                ctx["revision"].as_u64().ok_or_else(|| {
                    HostError::new("E_IO", "live-attach session context omitted revision")
                })?
            }
        }
    };
    let branch_policy = match trimmed_branch {
        None => "preview".to_owned(),
        Some("main") => "main".to_owned(),
        Some(name) => {
            let branches = listed_branches()?;
            if head_of(&branches, name).is_none() {
                return Err(HostError::new(
                    "E_UNKNOWN_BRANCH",
                    format!(
                        "branch `{name}` does not exist on the live-attached desktop host; pass --headless to fork a new named branch or omit --branch to target the default preview branch `{LIVE_DEFAULT_PREVIEW_BRANCH}`"
                    ),
                ));
            }
            format!("named:{name}")
        }
    };
    let payload = live.call_tool(
        "cad_apply_commands",
        json!({
            "expected_revision": expected,
            "actor": {
                "kind": "external_agent",
                "client": "cad-cli",
                "session_id": "api-apply",
            },
            "branch_policy": branch_policy,
            "commands": commands,
        }),
    )?;
    serde_json::from_value(payload).map_err(|err| {
        HostError::new(
            "E_IO",
            format!("cannot decode live-attach apply outcome: {err}"),
        )
    })
}

fn apply_batch(
    session: &mut DocumentSession,
    commands: Vec<SemanticCommand>,
    expected_revision: Option<u64>,
    branch: Option<&str>,
) -> Result<ExecOutcome, cad_runtime::RuntimeError> {
    let actor = Actor::ExternalAgent {
        client: "cad-cli".to_owned(),
        session_id: "api-apply".to_owned(),
    };
    let mut created_branch: Option<String> = None;
    let target = match branch.map(str::trim).filter(|name| !name.is_empty()) {
        None => BranchTarget::Active,
        Some("main") => BranchTarget::Main,
        Some(name) if name == session.active_branch() => BranchTarget::Active,
        Some(name) => {
            let branches = session.list_branches()?;
            let exists = branches.iter().any(|info| info.branch_name == name);
            if !exists {
                let main_rev = branches
                    .iter()
                    .find(|info| info.branch_name == "main")
                    .map(|info| u64::try_from(info.head_seq).unwrap_or(0))
                    .unwrap_or(session.revision());
                if let Some(expected) = expected_revision
                    && expected != main_rev
                {
                    // Validate against `main` before creating a branch so a stale revision
                    // never leaves an empty orphaned branch behind.
                    return session.exec_semantic_cmds(
                        Some(expected),
                        actor,
                        BranchTarget::Main,
                        Vec::new(),
                    );
                }
                session.branch_preview(name)?;
                created_branch = Some(name.to_owned());
            }
            BranchTarget::Named(name.to_owned())
        }
    };
    match session.exec_semantic_cmds(expected_revision, actor, target, commands) {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            if let Some(name) = created_branch {
                let _ = session.discard_preview(&name);
            }
            Err(err)
        }
    }
}

fn load_commands(spec: &str) -> Result<Vec<SemanticCommand>, HostError> {
    let text = if let Some(path) = spec.strip_prefix('@') {
        if path.is_empty() {
            return Err(HostError::new(
                "E400_INVALID_COMMAND",
                "commands @file path is empty",
            ));
        }
        fs::read_to_string(path).map_err(|err| {
            HostError::new("E_IO", format!("cannot read commands file `{path}`: {err}"))
        })?
    } else {
        spec.to_owned()
    };
    let value: Value = serde_json::from_str(text.trim()).map_err(|err| {
        HostError::new(
            "E400_INVALID_COMMAND",
            format!("commands are not JSON: {err}"),
        )
    })?;
    let list = if value.is_array() {
        value
    } else if value.is_object() {
        Value::Array(vec![value])
    } else {
        return Err(HostError::new(
            "E400_INVALID_COMMAND",
            "commands must be a SemanticCommand or a JSON array",
        ));
    };
    if list.as_array().is_some_and(Vec::is_empty) {
        return Err(HostError::new(
            "E400_INVALID_COMMAND",
            "commands must contain at least one SemanticCommand",
        ));
    }
    serde_json::from_value(list).map_err(|err| {
        HostError::new(
            "E400_INVALID_COMMAND",
            format!("invalid SemanticCommand batch: {err}"),
        )
    })
}

fn print_outcome(outcome: &ExecOutcome, json: bool) -> ExitCode {
    if json {
        return print_value(outcome);
    }
    println!(
        "applied revision {} on branch {}",
        outcome.revision, outcome.branch
    );
    if !outcome.summary.title.is_empty() {
        println!("{}", outcome.summary.title);
    }
    if !outcome.created_nodes.is_empty() {
        let ids: Vec<_> = outcome
            .created_nodes
            .iter()
            .map(|id| id.0.as_str())
            .collect();
        println!("created: {}", ids.join(", "));
    }
    ExitCode::SUCCESS
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

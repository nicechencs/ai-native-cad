//! Desktop-native AI profile store and `cad_agent.cli` bridge.
//!
//! API keys are written only under the user profile directory and are returned
//! to the webview as masks. A child process receives a key through its
//! environment, never through argv. Progress lines are Tauri events. Cancel
//! looks up the run by `trace_id`.

use crate::private_fs::{
    create_private_dir, path_is_private, strip_verbatim_prefix, write_private_file,
};
use cad_container::{AicadProject, ContainerManifest, KernelFingerprint, SqliteContainer};
use cad_kcl_bridge::{emit_kcl, parse_any_kcl_to_graph};
use cad_observe::ids::is_valid_id;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const AI_AGENT_PROGRESS_EVENT: &str = "cad-agent://progress";
pub const CLIENT_ABORTED: &str = "CLIENT_ABORTED";
const PROGRESS_PREFIX: &str = "CAD_AGENT_PROGRESS:";
const CONFIG_NAME: &str = "ai-config.json";

#[derive(Debug, Clone, Serialize)]
pub struct AiConfigStatus {
    pub default_provider: String,
    pub has_openai_key: bool,
    pub has_gemini_key: bool,
    pub openai_base_url: String,
    pub openai_model: String,
    pub gemini_model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfileClientView {
    pub id: String,
    pub name: String,
    pub preset: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub has_api_key: bool,
    pub api_key_mask: String,
    pub enable_vlm: bool,
    pub few_shot_k: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfilesBody {
    pub storage: String,
    pub active_profile_id: Option<String>,
    pub profiles: Vec<AiProfileClientView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_status: Option<AiConfigStatus>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfileDraft {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub enable_vlm: bool,
    #[serde(default)]
    pub few_shot_k: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAiProfilesRequest {
    #[serde(default)]
    pub active_profile_id: Option<String>,
    #[serde(default)]
    pub profiles: Vec<AiProfileDraft>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnectionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnectionResult {
    pub ok: bool,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub latency_ms: u128,
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, alias = "baseUrl")]
    pub base_url: Option<String>,
    #[serde(default, alias = "apiKey")]
    pub api_key: Option<String>,
    #[serde(default, alias = "profileId")]
    pub profile_id: Option<String>,
    #[serde(default, alias = "enableVlm")]
    pub enable_vlm: Option<bool>,
    #[serde(default, alias = "fewShotK")]
    pub few_shot_k: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentGenerateRequest {
    #[serde(default, alias = "traceId")]
    pub trace_id: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default, alias = "manufacturingMethod")]
    pub manufacturing_method: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default, alias = "existingSource")]
    pub existing_source: Option<String>,
    #[serde(default, alias = "workspaceRoot")]
    pub workspace_root: Option<String>,
    #[serde(default, alias = "projectPath")]
    pub project_path: Option<String>,
    #[serde(default, alias = "selectedTopoRefs")]
    pub selected_topo_refs: Vec<String>,
    #[serde(default, alias = "selectedNodeIds")]
    pub selected_node_ids: Vec<String>,
    #[serde(default)]
    pub llm: Option<LlmConfig>,
}

#[derive(Debug, Deserialize)]
pub struct CancelAgentRequest {
    #[serde(default, alias = "traceId")]
    pub trace_id: String,
}

#[derive(Clone)]
pub struct AgentPaths {
    pub python: String,
    pub agent_src: PathBuf,
    pub cad_cli: PathBuf,
    pub repo_root: PathBuf,
    pub work_dir: PathBuf,
    pub workspace_root: PathBuf,
}

pub struct PreparedAgent {
    pub trace_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub secrets: Vec<String>,
}

impl std::fmt::Debug for PreparedAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedAgent")
            .field("trace_id", &self.trace_id)
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub struct AgentRunOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRunResult {
    pub ok: bool,
    pub trace_id: String,
    pub exit_code: i32,
    pub payload: Value,
}

pub trait AgentRunner: Send + Sync {
    fn run(
        &self,
        prepared: &PreparedAgent,
        cancel: &AtomicBool,
        on_output: &mut dyn FnMut(OutputStream, &str),
    ) -> AgentRunOutput;
}

pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: &str, payload: &Value);
}

pub trait ConnectionProbe: Send + Sync {
    fn probe(&self, url: &str, header_name: &str, header_value: &str) -> Result<ProbeHttp, String>;
}

#[derive(Debug)]
pub struct ProbeHttp {
    pub status: u16,
    pub body: String,
}

pub struct OsConnectionProbe;

struct RunSlots {
    active: HashMap<String, Arc<AtomicBool>>,
    pending_cancel: HashSet<String>,
}

struct RunGuard(String);

impl Drop for RunGuard {
    fn drop(&mut self) {
        finish_run(&self.0);
    }
}

struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredProfile {
    id: String,
    name: String,
    preset: String,
    provider: String,
    model: String,
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    enable_vlm: bool,
    #[serde(default = "default_few_shot")]
    few_shot_k: i64,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileStoreFile {
    version: u32,
    active_profile_id: Option<String>,
    profiles: Vec<StoredProfile>,
}

fn default_few_shot() -> i64 {
    2
}

struct AppProgressSink(tauri::AppHandle);

impl ProgressSink for AppProgressSink {
    fn emit(&self, event: &str, payload: &Value) {
        use tauri::Emitter;
        if let Err(err) = self.0.emit(event, payload) {
            eprintln!("cad-agent progress was not delivered: {err}");
        }
    }
}

pub fn mask_api_key(api_key: &str) -> String {
    let raw = api_key.trim();
    if raw.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() <= 8 {
        return "••••••••".to_string();
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}••••{tail}")
}

pub fn ai_config_from_env(env: &BTreeMap<String, String>) -> AiConfigStatus {
    let has_openai_key = env_nonempty(env, "OPENAI_API_KEY");
    let has_gemini_key = env_nonempty(env, "GEMINI_API_KEY");
    let configured = env
        .get("CAD_AGENT_PROVIDER")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let default_provider = if matches!(configured.as_str(), "openai" | "gemini" | "template") {
        configured
    } else if has_openai_key {
        "openai".to_string()
    } else if has_gemini_key {
        "gemini".to_string()
    } else {
        "template".to_string()
    };
    AiConfigStatus {
        default_provider,
        has_openai_key,
        has_gemini_key,
        openai_base_url: env_or(env, "OPENAI_BASE_URL", "https://api.openai.com/v1"),
        openai_model: env_or(env, "OPENAI_MODEL", "gpt-4o-mini"),
        gemini_model: env_or(env, "GEMINI_MODEL", "gemini-2.5-flash"),
    }
}

pub fn default_store_path() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var("AICAD_STUDIO_AI_CONFIG_PATH")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(override_path));
    }
    Ok(home_dir()?.join(".aicad-studio").join(CONFIG_NAME))
}

pub fn load_ai_profiles_at(path: &Path) -> Result<AiProfilesBody, String> {
    let store = read_store(path)?;
    Ok(client_body(&store))
}

pub fn save_ai_profiles_at(
    path: &Path,
    request: &SaveAiProfilesRequest,
) -> Result<AiProfilesBody, String> {
    let existing = read_store(path)?;
    let existing_by_id: HashMap<_, _> = existing
        .profiles
        .iter()
        .map(|profile| (profile.id.clone(), profile.clone()))
        .collect();
    let mut next = Vec::new();
    for draft in request.profiles.iter().filter(|_| true).take(30) {
        let previous = existing_by_id.get(draft.id.trim());
        next.push(normalize_profile(draft, previous));
    }
    let requested = request
        .active_profile_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let active_profile_id = requested
        .filter(|id| next.iter().any(|profile| profile.id == *id))
        .map(str::to_string)
        .or_else(|| next.first().map(|profile| profile.id.clone()));
    let store = ProfileStoreFile {
        version: 1,
        active_profile_id,
        profiles: next,
    };
    write_private_store(path, &store)?;
    Ok(client_body(&store))
}

pub fn apply_saved_profile(
    request: &mut AgentGenerateRequest,
    store_path: &Path,
) -> Result<(), String> {
    let Some(llm) = request.llm.as_mut() else {
        return Ok(());
    };
    let profile_id = llm
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let Some(profile_id) = profile_id else {
        return Ok(());
    };
    let Some(profile) = read_store(store_path)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
    else {
        return Ok(());
    };
    if llm
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "auto")
        .is_none()
    {
        llm.provider = Some(profile.provider);
    }
    if llm
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        && !profile.model.trim().is_empty()
    {
        llm.model = Some(profile.model);
    }
    if llm
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        && !profile.base_url.trim().is_empty()
    {
        llm.base_url = Some(profile.base_url);
    }
    if usable_secret(llm.api_key.as_deref()).is_none() {
        if let Some(saved) = usable_secret(Some(profile.api_key.as_str())) {
            llm.api_key = Some(saved);
        }
    }
    if llm.enable_vlm.is_none() {
        llm.enable_vlm = Some(profile.enable_vlm);
    }
    if llm.few_shot_k.is_none() {
        llm.few_shot_k = Some(profile.few_shot_k);
    }
    Ok(())
}

pub struct AgentCliArgsInput<'a> {
    pub prompt: &'a str,
    pub material: &'a str,
    pub manufacturing_method: &'a str,
    pub mode: &'a str,
    pub provider: &'a str,
    pub work_dir: &'a Path,
    pub cad_cli: &'a Path,
    pub workspace_root: &'a Path,
    pub existing_kcl_path: Option<&'a Path>,
    pub project_path: Option<&'a str>,
    pub target: Option<&'a str>,
    pub llm: &'a LlmConfig,
    pub cli_help: &'a str,
    pub topo_refs: &'a [String],
    pub node_ids: &'a [String],
}

pub fn build_agent_cli_args(input: &AgentCliArgsInput<'_>) -> Result<Vec<String>, String> {
    let mut args = vec![
        "-m".to_string(),
        "cad_agent.cli".to_string(),
        "--semantic-v2".to_string(),
        format!("--prompt={}", input.prompt),
        format!("--material={}", input.material),
        format!("--manufacturing-method={}", input.manufacturing_method),
        format!("--mode={}", input.mode),
        format!("--provider={}", input.provider),
        "--work-dir".to_string(),
        path_string(&strip_verbatim_prefix(input.work_dir)),
        "--cli-bin".to_string(),
        path_string(&strip_verbatim_prefix(input.cad_cli)),
        "--workspace-root".to_string(),
        path_string(&strip_verbatim_prefix(input.workspace_root)),
        "--json".to_string(),
    ];
    if let Some(existing_path) = input.existing_kcl_path {
        args.push("--existing-kcl".to_string());
        args.push(path_string(&strip_verbatim_prefix(existing_path)));
    }
    if let Some(target) = input.target {
        args.push("--target".to_string());
        args.push(target.to_string());
    }
    if let Some(model) = input
        .llm
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        args.push(format!("--model={}", bounded_text(Some(model), "", 120)));
    }
    if let Some(base_url) = input
        .llm
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if base_url.chars().any(char::is_control) {
            return Err("Invalid 'llm.base_url': contains control characters".to_string());
        }
        args.push(format!("--base-url={}", bounded_text(Some(base_url), "", 300)));
    }
    if input.llm.enable_vlm == Some(true) {
        args.push("--enable-vlm".to_string());
    }
    if let Some(few_shot) = input.llm.few_shot_k {
        args.push(format!("--few-shot-k={few_shot}"));
    }
    if input.cli_help.contains("--progress-stderr") {
        args.push("--progress-stderr".to_string());
    }
    // Today's CLI stores one project path in `--project`. Full selection lists
    // always travel in the environment because `--target` accepts only one id.
    if let Some(project) = input.project_path {
        args.push("--project".to_string());
        args.push(project.to_string());
    }
    push_selection_args(
        &mut args,
        input.cli_help,
        "--selected-topo-refs",
        "--selected-topo-ref",
        input.topo_refs,
    );
    push_selection_args(
        &mut args,
        input.cli_help,
        "--selected-node-ids",
        "--selected-node-id",
        input.node_ids,
    );
    Ok(args)
}

pub fn prepare_agent_invocation(
    request: &AgentGenerateRequest,
    paths: &AgentPaths,
    cli_help: &str,
    base_env: &BTreeMap<String, String>,
) -> Result<PreparedAgent, String> {
    let trace_id = request.trace_id.trim();
    if !is_valid_id(trace_id) {
        return Err(
            "trace_id must be 8-64 characters of letters, digits, '_' or '-'".to_string(),
        );
    }
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err("Missing non-empty 'prompt' field".to_string());
    }
    if prompt.chars().count() > 32_000 {
        return Err("prompt is too long".to_string());
    }
    let mode = request.mode.as_deref().unwrap_or("generate").trim();
    if mode != "generate" && mode != "edit" {
        return Err("Invalid 'mode': expected 'generate' or 'edit'".to_string());
    }
    let (provider, api_key) = resolve_provider(request, base_env)?;
    let llm = request.llm.clone().unwrap_or_default();
    if let Some(few_shot) = llm.few_shot_k
        && !(0..=5).contains(&few_shot)
    {
        return Err("Invalid 'llm.few_shot_k': expected integer between 0 and 5".to_string());
    }
    let topo_refs = clean_ids(&request.selected_topo_refs, "selected_topo_refs")?;
    let node_ids = clean_ids(&request.selected_node_ids, "selected_node_ids")?;
    let project_path = match clean_optional_path(request.project_path.as_deref(), "project_path")? {
        Some(raw) => Some(resolve_project_in_workspace(&paths.workspace_root, &raw)?),
        None => None,
    };
    let material = bounded_text(request.material.as_deref(), "Al-6061-T6", 80);
    let method = bounded_text(
        request.manufacturing_method.as_deref(),
        "CNC_3AXIS_MILLING",
        80,
    );
    fs::create_dir_all(&paths.work_dir)
        .map_err(|err| format!("Failed to create agent work dir: {err}"))?;

    let mut existing_kcl_path: Option<PathBuf> = None;
    let mut target: Option<&str> = None;
    if mode == "edit" {
        let source = request.existing_source.as_deref().unwrap_or("");
        if !source.trim().is_empty() {
            let staged = paths.work_dir.join("existing_input.kcl");
            fs::write(&staged, source)
                .map_err(|err| format!("Failed to stage existing KCL: {err}"))?;
            existing_kcl_path = Some(staged);
        } else if project_path.is_none() {
            return Err(
                "Edit mode requires non-empty 'existing_source' or a V2 'project_path'".to_string(),
            );
        }
        if project_path.is_some()
            && cli_help.contains("--target")
            && let Some(selected) = topo_refs.first().or_else(|| node_ids.first())
        {
            target = Some(selected.as_str());
        }
    }

    let args = build_agent_cli_args(&AgentCliArgsInput {
        prompt,
        material: &material,
        manufacturing_method: &method,
        mode,
        provider: &provider,
        work_dir: &paths.work_dir,
        cad_cli: &paths.cad_cli,
        workspace_root: &paths.workspace_root,
        existing_kcl_path: existing_kcl_path.as_deref(),
        project_path: project_path.as_deref(),
        target,
        llm: &llm,
        cli_help,
        topo_refs: &topo_refs,
        node_ids: &node_ids,
    })?;
    if let Some(key) = api_key.as_deref()
        && key.len() >= 12
        && (args.iter().any(|arg| arg.contains(key)) || paths.python.contains(key))
    {
        return Err("Refusing to pass the API key on the agent command line".to_string());
    }

    let mut env = base_env.clone();
    env.insert(
        "PYTHONPATH".to_string(),
        path_string(&strip_verbatim_prefix(&paths.agent_src)),
    );
    env.insert("PYTHONUTF8".to_string(), "1".to_string());
    env.insert(
        "AICAD_PROJECT_PATH".to_string(),
        project_path.clone().unwrap_or_default(),
    );
    env.insert(
        "AICAD_SELECTED_TOPO_REFS".to_string(),
        serde_json::to_string(&topo_refs).unwrap_or_else(|_| "[]".to_string()),
    );
    env.insert(
        "AICAD_SELECTED_NODE_IDS".to_string(),
        serde_json::to_string(&node_ids).unwrap_or_else(|_| "[]".to_string()),
    );
    match provider.as_str() {
        "gemini" => {
            env.remove("CAD_AGENT_PROVIDER");
            env.remove("OPENAI_API_KEY");
            if let Some(key) = api_key.clone() {
                env.insert("GEMINI_API_KEY".to_string(), key);
            }
        }
        "openai" => {
            env.remove("CAD_AGENT_PROVIDER");
            env.remove("GEMINI_API_KEY");
            if let Some(key) = api_key.clone() {
                env.insert("OPENAI_API_KEY".to_string(), key);
            }
        }
        _ => {}
    }
    let secrets = collect_secrets(&env, api_key.as_deref());
    Ok(PreparedAgent {
        trace_id: trace_id.to_string(),
        program: paths.python.clone(),
        args,
        env,
        cwd: strip_verbatim_prefix(&paths.repo_root),
        secrets,
    })
}

pub fn parse_agent_progress_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    let json_text = trimmed.strip_prefix(PROGRESS_PREFIX)?.trim();
    if json_text.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(json_text).ok()?;
    let object = parsed.as_object()?;
    if !object.get("stage")?.is_string() || !object.get("message")?.is_string() {
        return None;
    }
    let mut event = parsed;
    if let Some(map) = event.as_object_mut() {
        map.insert("event".to_string(), json!("progress"));
    }
    Some(event)
}

pub fn run_prepared_agent(
    prepared: &PreparedAgent,
    runner: &dyn AgentRunner,
    sink: &dyn ProgressSink,
) -> Result<AgentRunResult, String> {
    if !is_valid_id(prepared.trace_id.trim()) {
        return Err(
            "trace_id must be 8-64 characters of letters, digits, '_' or '-'".to_string(),
        );
    }
    let flag = register_run(prepared.trace_id.trim());
    let _guard = RunGuard(prepared.trace_id.clone());
    if flag.load(Ordering::SeqCst) {
        return Err(CLIENT_ABORTED.to_string());
    }
    let secrets = prepared.secrets.clone();
    let trace_id = prepared.trace_id.clone();
    let mut on_output = |stream: OutputStream, line: &str| {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        let _ = stream;
        if let Some(mut event) = parse_agent_progress_line(line) {
            redact_value(&mut event, &secrets);
            if let Some(map) = event.as_object_mut() {
                map.insert("trace_id".to_string(), json!(trace_id));
            }
            sink.emit(AI_AGENT_PROGRESS_EVENT, &event);
        }
    };
    let output = runner.run(prepared, flag.as_ref(), &mut on_output);
    if flag.load(Ordering::SeqCst) || output.cancelled {
        return Err(CLIENT_ABORTED.to_string());
    }
    let stdout = output.stdout.trim();
    if stdout.is_empty() {
        return Err(redact_text(
            &format!(
                "cad_agent.cli failed (exit {}): {}",
                output.exit_code,
                clip(output.stderr.trim())
            ),
            &secrets,
        ));
    }
    let mut payload: Value = serde_json::from_str(stdout).map_err(|_| {
        redact_text(
            &format!(
                "cad_agent.cli did not return JSON (exit {}): {}",
                output.exit_code,
                clip(if output.stderr.trim().is_empty() {
                    stdout
                } else {
                    output.stderr.trim()
                })
            ),
            &secrets,
        )
    })?;
    if !payload.is_object() {
        return Err("cad_agent.cli JSON result must be an object".to_string());
    }
    redact_value(&mut payload, &secrets);
    if let Some(map) = payload.as_object_mut() {
        map.entry("trace_id".to_string())
            .or_insert_with(|| json!(prepared.trace_id));
    }
    let verdict = payload
        .get("overall_status")
        .and_then(Value::as_str)
        .or_else(|| payload.get("verdict").and_then(Value::as_str))
        .or_else(|| {
            payload
                .get("verification")
                .and_then(|v| v.get("overall_status"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let has_error = payload
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
        || payload
            .get("error_code")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
    let zero_trust_unknown = verdict.eq_ignore_ascii_case("UNKNOWN") && !has_error;
    let ok = payload
        .get("ok")
        .and_then(Value::as_bool)
        .or_else(|| payload.get("passed").and_then(Value::as_bool))
        .map(|passed| passed || zero_trust_unknown)
        .unwrap_or_else(|| (output.exit_code == 0 || zero_trust_unknown) && !has_error);
    Ok(AgentRunResult {
        ok,
        trace_id: prepared.trace_id.clone(),
        exit_code: output.exit_code,
        payload,
    })
}

pub fn cancel_agent_run(trace_id: &str) -> bool {
    let id = trace_id.trim();
    if !is_valid_id(id) {
        return false;
    }
    let mut slots = lock_slots();
    if let Some(flag) = slots.active.get(id) {
        flag.store(true, Ordering::SeqCst);
        return true;
    }
    if slots.pending_cancel.len() > 256 {
        slots.pending_cancel.clear();
    }
    slots.pending_cancel.insert(id.to_string());
    true
}

pub fn test_ai_connection_with(
    request: &AiConnectionRequest,
    store_path: Option<&Path>,
    env: &BTreeMap<String, String>,
    probe: &dyn ConnectionProbe,
) -> AiConnectionResult {
    let started = Instant::now();
    let saved = if let Some(path) = store_path {
        let profile_id = request.profile_id.as_deref().map(str::trim).unwrap_or("");
        if profile_id.is_empty() {
            None
        } else {
            match read_store(path) {
                Ok(store) => store
                    .profiles
                    .into_iter()
                    .find(|profile| profile.id == profile_id),
                Err(error) => {
                    return AiConnectionResult {
                        ok: false,
                        provider: request.provider.clone(),
                        model: None,
                        status: None,
                        latency_ms: started.elapsed().as_millis(),
                        message: error,
                    };
                }
            }
        }
    } else {
        None
    };
    let mut provider = normalize_provider(
        request
            .provider
            .trim()
            .is_empty()
            .then(|| saved.as_ref().map(|profile| profile.provider.as_str()))
            .flatten()
            .unwrap_or(request.provider.trim()),
    );
    if request.provider.trim().is_empty() {
        if let Some(saved) = saved.as_ref() {
            provider = normalize_provider(&saved.provider);
        }
    } else {
        provider = normalize_provider(request.provider.trim());
    }
    let model_input = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            saved.as_ref().and_then(|profile| {
                let model = profile.model.trim();
                (!model.is_empty()).then(|| model.to_string())
            })
        });
    let base_url_input = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            saved.as_ref().and_then(|profile| {
                let base_url = profile.base_url.trim();
                (!base_url.is_empty()).then(|| base_url.to_string())
            })
        });
    let direct_key = if request.clear_api_key {
        None
    } else {
        usable_secret(request.api_key.as_deref())
    };
    let mut api_key = direct_key.or_else(|| {
        if request.clear_api_key {
            None
        } else {
            saved
                .as_ref()
                .and_then(|profile| usable_secret(Some(profile.api_key.as_str())))
        }
    });
    if provider == "auto" {
        provider = if base_url_input
            .as_deref()
            .is_some_and(|url| url.to_ascii_lowercase().contains("generativelanguage.googleapis.com"))
        {
            "gemini".to_string()
        } else if model_input
            .as_deref()
            .is_some_and(|model| model.to_ascii_lowercase().contains("gemini"))
            || api_key.as_deref().is_some_and(|key| key.starts_with("AIza"))
        {
            "gemini".to_string()
        } else if api_key.is_some() || base_url_input.is_some() {
            "openai".to_string()
        } else if env_nonempty(env, "OPENAI_API_KEY") {
            "openai".to_string()
        } else if env_nonempty(env, "GEMINI_API_KEY") {
            "gemini".to_string()
        } else {
            return AiConnectionResult {
                ok: false,
                provider: "auto".to_string(),
                model: model_input,
                status: None,
                latency_ms: started.elapsed().as_millis(),
                message: "未配置真实 LLM API Key，已禁用离线假模板模式。请填写 OpenAI / Gemini API Key、本地兼容端点或配置环境变量。".to_string(),
            };
        };
    }
    if provider == "template" {
        return AiConnectionResult {
            ok: false,
            provider,
            model: None,
            status: None,
            latency_ms: started.elapsed().as_millis(),
            message: "已禁用离线假模板模式，请配置真实 LLM（OpenAI / Gemini）API Key 或本地兼容端点。".to_string(),
        };
    }
    if api_key.is_none() {
        let env_key = if provider == "gemini" {
            "GEMINI_API_KEY"
        } else {
            "OPENAI_API_KEY"
        };
        api_key = env
            .get(env_key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    let secrets = collect_secrets(env, api_key.as_deref());
    if provider == "gemini" {
        let model = model_input
            .clone()
            .or_else(|| {
                env.get("GEMINI_MODEL")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| "gemini-2.5-flash".to_string());
        let Some(key) = api_key else {
            return AiConnectionResult {
                ok: false,
                provider,
                model: Some(model),
                status: None,
                latency_ms: started.elapsed().as_millis(),
                message: "未提供 Gemini API Key，请填写密钥或配置服务端 GEMINI_API_KEY。".to_string(),
            };
        };
        if key_has_header_break(&key) {
            return connection_failure(provider, Some(model), started, "API Key 含有非法换行。", &secrets);
        }
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}",
            encode_path_segment(&model)
        );
        return finish_probe(
            probe.probe(&url, "x-goog-api-key", &key),
            provider,
            model,
            started,
            &secrets,
            true,
        );
    }
    let base_url = base_url_input
        .clone()
        .or_else(|| {
            env.get("OPENAI_BASE_URL")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let base_url = base_url.trim_end_matches('/').to_string();
    if base_url.chars().any(char::is_control)
        || !(base_url.starts_with("http://") || base_url.starts_with("https://"))
    {
        return connection_failure(
            provider,
            model_input,
            started,
            "Base URL 格式非法（需以 http:// 或 https:// 开头且不含控制字符）。",
            &secrets,
        );
    }
    let model = model_input
        .clone()
        .or_else(|| {
            env.get("OPENAI_MODEL")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    if api_key.is_none() && !is_local_endpoint(&base_url) {
        return AiConnectionResult {
            ok: false,
            provider,
            model: Some(model),
            status: None,
            latency_ms: started.elapsed().as_millis(),
            message: "未提供 API Key（非本地端点需要填写 API Key 或配置 OPENAI_API_KEY）。".to_string(),
        };
    }
    if api_key.as_deref().is_some_and(key_has_header_break) {
        return connection_failure(provider, Some(model), started, "API Key 含有非法换行。", &secrets);
    }
    let header_value = api_key
        .as_deref()
        .map(|key| format!("Bearer {key}"))
        .unwrap_or_default();
    let url = format!("{base_url}/models");
    finish_probe(
        probe.probe(&url, "Authorization", &header_value),
        provider,
        model,
        started,
        &secrets,
        false,
    )
}

impl ConnectionProbe for OsConnectionProbe {
    fn probe(&self, url: &str, header_name: &str, header_value: &str) -> Result<ProbeHttp, String> {
        if url.chars().any(char::is_control)
            || !(url.starts_with("http://") || url.starts_with("https://"))
        {
            return Err("Unsupported probe URL".to_string());
        }
        probe_http_via_python(url, header_name, header_value)
    }
}

pub struct SystemAgentRunner;

impl AgentRunner for SystemAgentRunner {
    fn run(
        &self,
        prepared: &PreparedAgent,
        cancel: &AtomicBool,
        on_output: &mut dyn FnMut(OutputStream, &str),
    ) -> AgentRunOutput {
        if cancel.load(Ordering::SeqCst) {
            return AgentRunOutput {
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                cancelled: true,
            };
        }
        let mut command = Command::new(&prepared.program);
        command
            .args(&prepared.args)
            .current_dir(&prepared.cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &prepared.env {
            command.env(key, value);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                return AgentRunOutput {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("failed to start cad_agent.cli: {err}"),
                    cancelled: false,
                };
            }
        };
        let (tx, rx) = std::sync::mpsc::channel::<(OutputStream, String)>();
        let stdout_tx = tx.clone();
        let stderr_tx = tx;
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_thread = thread::spawn(move || {
            if let Some(pipe) = stdout_pipe {
                read_pipe_lines(pipe, OutputStream::Stdout, &stdout_tx);
            }
        });
        let stderr_thread = thread::spawn(move || {
            if let Some(pipe) = stderr_pipe {
                read_pipe_lines(pipe, OutputStream::Stderr, &stderr_tx);
            }
        });
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = -1;
        let mut cancelled = false;
        loop {
            if cancel.load(Ordering::SeqCst) {
                kill_child_tree(&mut child);
                cancelled = true;
                break;
            }
            while let Ok((stream, line)) = rx.try_recv() {
                record_line(stream, &line, &mut stdout, &mut stderr);
                on_output(stream, &line);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_code = status.code().unwrap_or(1);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(err) => {
                    stderr.push_str(&format!("cad_agent.cli wait failed: {err}"));
                    break;
                }
            }
        }
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        while let Ok((stream, line)) = rx.try_recv() {
            record_line(stream, &line, &mut stdout, &mut stderr);
            if !cancelled {
                on_output(stream, &line);
            }
        }
        AgentRunOutput {
            exit_code,
            stdout,
            stderr,
            cancelled,
        }
    }
}

pub(crate) fn resolve_cad_cli_bin() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var_os("CAD_CLI_BIN").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "CAD_CLI_BIN does not point at a cad-cli binary: {}",
            path.display()
        ));
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("cad-cli.exe"));
        candidates.push(dir.join("cad-cli"));
    }
    let repo = repo_root();
    for build in ["release", "debug"] {
        let dir = repo.join("target").join(build);
        candidates.push(dir.join("cad-cli.exe"));
        candidates.push(dir.join("cad-cli"));
    }
    newest_file(&candidates).ok_or_else(|| {
        "Compiled cad-cli binary not found. Build it with cargo build -p cad-cli, or set CAD_CLI_BIN."
            .to_string()
    })
}

pub(crate) fn kill_child_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[tauri::command]
pub fn get_ai_config() -> AiConfigStatus {
    ai_config_from_env(&current_env())
}

#[tauri::command]
pub fn get_ai_profiles() -> Result<AiProfilesBody, String> {
    let mut body = load_ai_profiles_at(&default_store_path()?)?;
    body.env_status = Some(ai_config_from_env(&current_env()));
    Ok(body)
}

#[tauri::command]
pub fn save_ai_profiles(request: SaveAiProfilesRequest) -> Result<AiProfilesBody, String> {
    let mut body = save_ai_profiles_at(&default_store_path()?, &request)?;
    body.env_status = Some(ai_config_from_env(&current_env()));
    Ok(body)
}

#[tauri::command]
pub fn test_ai_connection(request: AiConnectionRequest) -> Result<AiConnectionResult, String> {
    let path = default_store_path()?;
    Ok(test_ai_connection_with(
        &request,
        Some(path.as_path()),
        &current_env(),
        &OsConnectionProbe,
    ))
}

#[tauri::command]
pub async fn run_ai_agent(
    request: AgentGenerateRequest,
    workspace: tauri::State<'_, crate::DesktopWorkspace>,
    app: tauri::AppHandle,
) -> Result<AgentRunResult, String> {
    let root = workspace
        .authorized_root(request.workspace_root.as_deref())?
        .to_path_buf();
    let sink = AppProgressSink(app);
    tauri::async_runtime::spawn_blocking(move || run_ai_agent_blocking(request, &root, &sink))
        .await
        .map_err(|err| err.to_string())?
}

#[tauri::command]
pub fn cancel_ai_agent(request: CancelAgentRequest) -> bool {
    cancel_agent_run(&request.trace_id)
}

fn run_ai_agent_blocking(
    mut request: AgentGenerateRequest,
    workspace_root: &Path,
    sink: &dyn ProgressSink,
) -> Result<AgentRunResult, String> {
    let store = default_store_path()?;
    apply_saved_profile(&mut request, &store)?;
    let python = resolve_python_bin()?;
    let cad_cli = resolve_cad_cli_bin()?;
    let repo = repo_root();
    let agent_src = repo.join("services").join("cad-agent").join("src");
    if !agent_src.join("cad_agent").join("cli.py").is_file() {
        return Err("cad_agent.cli was not found under services/cad-agent/src".to_string());
    }
    let work_dir = unique_dir("aicad-agent");
    fs::create_dir_all(&work_dir)
        .map_err(|err| format!("Failed to create agent work dir: {err}"))?;
    let _cleanup = DirGuard(work_dir.clone());
    let mut _ephemeral_project_cleanup: Option<DirGuard> = None;
    let mut ephemeral_project_file: Option<PathBuf> = None;
    if request
        .project_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        let ephemeral_name = unique_dir_name(".aicad-v2-tmp");
        let ephemeral_dir = strip_verbatim_prefix(workspace_root).join(&ephemeral_name);
        fs::create_dir_all(&ephemeral_dir)
            .map_err(|err| format!("Failed to create ephemeral V2 project dir: {err}"))?;
        let session_file = ephemeral_dir.join("session.aicad");
        seed_ephemeral_v2_project(&session_file, &mut request)?;
        request.project_path = Some(format!("{ephemeral_name}/session.aicad"));
        ephemeral_project_file = Some(session_file);
        _ephemeral_project_cleanup = Some(DirGuard(ephemeral_dir));
    }
    let paths = AgentPaths {
        python,
        agent_src: strip_verbatim_prefix(&agent_src),
        cad_cli: strip_verbatim_prefix(&cad_cli),
        repo_root: strip_verbatim_prefix(&repo),
        work_dir: strip_verbatim_prefix(&work_dir),
        workspace_root: strip_verbatim_prefix(workspace_root),
    };
    let help = probe_agent_help(&paths);
    let prepared = prepare_agent_invocation(&request, &paths, &help, &current_env())?;
    let mut result = run_prepared_agent(&prepared, &SystemAgentRunner, sink)?;
    if let Some(session_file) = ephemeral_project_file.as_deref() {
        hydrate_ephemeral_v2_result(&mut result, session_file);
    }
    Ok(result)
}

fn seed_ephemeral_v2_project(
    session_file: &Path,
    request: &mut AgentGenerateRequest,
) -> Result<(), String> {
    let mode = request
        .mode
        .as_deref()
        .map(str::trim)
        .unwrap_or("generate");
    if !mode.eq_ignore_ascii_case("edit") {
        return Ok(());
    }
    let existing = request.existing_source.as_deref().unwrap_or("").trim();
    if existing.is_empty() {
        return Ok(());
    }
    let graph = parse_any_kcl_to_graph(existing)
        .map_err(|err| format!("Failed to parse existing KCL for ephemeral V2 edit: {err}"))?;
    if request.selected_topo_refs.is_empty()
        && request.selected_node_ids.is_empty()
        && let Some(default_node) = graph
            .outputs
            .first()
            .map(|id| id.0.clone())
            .or_else(|| graph.nodes.keys().next().cloned())
    {
        request.selected_node_ids.push(default_node);
    }
    let mut project = AicadProject::new(
        ContainerManifest::new(
            "ephemeral_edit",
            KernelFingerprint::new("analytic", env!("CARGO_PKG_VERSION")),
        ),
        graph,
    );
    project.kcl_view = Some(existing.to_string());
    SqliteContainer::create(session_file, &project)
        .map_err(|err| format!("Failed to seed ephemeral V2 project: {err}"))?;
    Ok(())
}

fn hydrate_ephemeral_v2_result(result: &mut AgentRunResult, session_file: &Path) {
    let has_source = result
        .payload
        .get("source")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
        || result
            .payload
            .get("kcl_code")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
    if !has_source && session_file.is_file() {
        if let Ok(container) = SqliteContainer::open(session_file) {
            let branch = result
                .payload
                .get("preview_branch")
                .and_then(Value::as_str)
                .or_else(|| result.payload.get("branch").and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| container.active_branch().ok());
            if let Some(branch_name) = branch
                && let Ok(graph) = container.load_branch_graph(&branch_name)
                && let Ok(kcl) = emit_kcl(&graph)
                && !kcl.trim().is_empty()
                && let Some(map) = result.payload.as_object_mut()
            {
                map.insert("source".to_string(), json!(kcl));
            }
        }
    }
    if let Some(map) = result.payload.as_object_mut() {
        // Ephemeral temp container is removed as soon as this call returns;
        // omit its path so preview commit/discard applies in-memory KCL cleanly.
        map.remove("project_path");
        map.insert("ephemeral_project".to_string(), json!(true));
    }
}

fn resolve_python_bin() -> Result<String, String> {
    let mut candidates = Vec::new();
    if let Some(override_bin) = std::env::var("CAD_AGENT_PYTHON")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        candidates.push(override_bin);
    }
    candidates.extend(["python".to_string(), "python3".to_string(), "py".to_string()]);
    for candidate in candidates {
        let status = Command::new(&candidate)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.is_ok_and(|code| code.success()) {
            return Ok(candidate);
        }
    }
    Err("Python was not found. Install Python 3.12+ or set CAD_AGENT_PYTHON.".to_string())
}

fn probe_agent_help(paths: &AgentPaths) -> String {
    let output = Command::new(&paths.python)
        .args(["-m", "cad_agent.cli", "--help"])
        .current_dir(strip_verbatim_prefix(&paths.repo_root))
        .env("PYTHONPATH", strip_verbatim_prefix(&paths.agent_src))
        .env("PYTHONUTF8", "1")
        .output();
    match output {
        Ok(output) => format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(_) => String::new(),
    }
}

pub fn repo_root() -> PathBuf {
    if let Some(override_root) = std::env::var_os("AICAD_REPO_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return strip_verbatim_prefix(&fs::canonicalize(&override_root).unwrap_or(override_root));
    }

    let mut seeds = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        seeds.push(dir.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        seeds.push(cwd);
    }

    for seed in &seeds {
        let mut cursor = Some(seed.as_path());
        for _ in 0..10 {
            let Some(dir) = cursor else {
                break;
            };
            if is_repo_root_candidate(dir) {
                return strip_verbatim_prefix(&fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
            }
            cursor = dir.parent();
        }
    }

    let fallback = seeds
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("."));
    strip_verbatim_prefix(&fs::canonicalize(&fallback).unwrap_or(fallback))
}

fn is_repo_root_candidate(dir: &Path) -> bool {
    dir.join("services")
        .join("cad-agent")
        .join("src")
        .join("cad_agent")
        .join("cli.py")
        .is_file()
        || (dir.join("Cargo.toml").is_file()
            && dir.join("crates").is_dir()
            && dir.join("apps").is_dir())
}

fn current_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn home_dir() -> Result<PathBuf, String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("Could not resolve the user home directory from {key}"))
}

fn env_nonempty(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| !value.trim().is_empty())
}

fn env_or(env: &BTreeMap<String, String>, key: &str, default: &str) -> String {
    env.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn client_body(store: &ProfileStoreFile) -> AiProfilesBody {
    AiProfilesBody {
        storage: "server-file".to_string(),
        active_profile_id: store.active_profile_id.clone(),
        profiles: store.profiles.iter().map(client_profile).collect(),
        env_status: None,
    }
}

fn client_profile(profile: &StoredProfile) -> AiProfileClientView {
    let api_key = profile.api_key.trim();
    AiProfileClientView {
        id: profile.id.clone(),
        name: profile.name.clone(),
        preset: profile.preset.clone(),
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        base_url: profile.base_url.clone(),
        has_api_key: !api_key.is_empty(),
        api_key_mask: mask_api_key(api_key),
        enable_vlm: profile.enable_vlm,
        few_shot_k: profile.few_shot_k,
        updated_at: profile.updated_at.clone(),
    }
}

fn read_store(path: &Path) -> Result<ProfileStoreFile, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProfileStoreFile {
                version: 1,
                active_profile_id: None,
                profiles: Vec::new(),
            });
        }
        Err(error) => {
            return Err(format!(
                "Could not read AI profile store {}: {error}",
                path.display()
            ));
        }
    };
    let parsed = serde_json::from_str::<ProfileStoreFile>(&text).map_err(|error| {
        format!(
            "AI profile store {} is unreadable ({error}). The file was not replaced.",
            path.display()
        )
    })?;
    let profiles: Vec<_> = parsed
        .profiles
        .iter()
        .map(|profile| normalize_stored(profile))
        .collect();
    let active = parsed
        .active_profile_id
        .filter(|id| profiles.iter().any(|profile| profile.id == *id))
        .or_else(|| profiles.first().map(|profile| profile.id.clone()));
    Ok(ProfileStoreFile {
        version: 1,
        active_profile_id: active,
        profiles,
    })
}

fn normalize_stored(profile: &StoredProfile) -> StoredProfile {
    let draft = AiProfileDraft {
        id: profile.id.clone(),
        name: profile.name.clone(),
        preset: profile.preset.clone(),
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        base_url: profile.base_url.clone(),
        api_key: Some(profile.api_key.clone()),
        clear_api_key: false,
        enable_vlm: profile.enable_vlm,
        few_shot_k: Some(profile.few_shot_k),
        updated_at: Some(profile.updated_at.clone()),
    };
    normalize_profile(&draft, None)
}

fn normalize_profile(raw: &AiProfileDraft, existing: Option<&StoredProfile>) -> StoredProfile {
    let provider = normalize_provider(
        raw.provider
            .trim()
            .is_empty()
            .then(|| existing.map(|profile| profile.provider.as_str()))
            .flatten()
            .unwrap_or(raw.provider.trim()),
    );
    let preset = normalize_preset(
        if raw.preset.trim().is_empty() {
            existing.map(|profile| profile.preset.as_str()).unwrap_or("")
        } else {
            raw.preset.trim()
        },
        &provider,
    );
    let id = {
        let incoming = raw.id.trim();
        if !incoming.is_empty() {
            truncate_chars(incoming, 64)
        } else if let Some(existing) = existing {
            existing.id.clone()
        } else {
            new_profile_id()
        }
    };
    let name = {
        let incoming = raw.name.trim();
        if !incoming.is_empty() {
            truncate_chars(incoming, 80)
        } else {
            existing
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| "未命名模型配置".to_string())
        }
    };
    let model = truncate_chars(raw.model.trim(), 120);
    let base_url = truncate_chars(raw.base_url.trim(), 300);
    let model = if model.is_empty() {
        existing.map(|profile| profile.model.clone()).unwrap_or_default()
    } else {
        model
    };
    let base_url = if raw.base_url.trim().is_empty() && existing.is_some() {
        existing
            .map(|profile| profile.base_url.clone())
            .unwrap_or_default()
    } else {
        base_url
    };
    // Draft JSON always includes enableVlm. An omitted boolean deserializes as false.
    let enable_vlm = raw.enable_vlm;
    let few_shot_k = raw
        .few_shot_k
        .filter(|value| (0..=5).contains(value))
        .or_else(|| existing.map(|profile| profile.few_shot_k))
        .unwrap_or(2);
    let mut api_key = existing
        .map(|profile| profile.api_key.clone())
        .unwrap_or_default();
    if raw.clear_api_key {
        api_key.clear();
    } else if let Some(incoming) = raw.api_key.as_deref() {
        let trimmed = incoming.trim();
        if !trimmed.is_empty() && !trimmed.contains('•') {
            api_key = trimmed.to_string();
        }
    }
    let updated_at = raw
        .updated_at
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    StoredProfile {
        id,
        name,
        preset,
        provider,
        model,
        base_url,
        api_key,
        enable_vlm,
        few_shot_k,
        updated_at,
    }
}

fn normalize_provider(value: &str) -> String {
    match value.trim() {
        "auto" | "template" | "openai" | "gemini" => value.trim().to_string(),
        _ => "auto".to_string(),
    }
}

fn normalize_preset(value: &str, provider: &str) -> String {
    if matches!(
        value,
        "openai" | "gemini" | "deepseek" | "openrouter" | "ollama" | "custom" | "template"
    ) {
        return value.to_string();
    }
    match provider {
        "gemini" => "gemini".to_string(),
        "template" => "template".to_string(),
        "openai" => "openai".to_string(),
        _ => "custom".to_string(),
    }
}

fn new_profile_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("prof_{millis:x}{seq:x}")
}

fn write_private_store(path: &Path, store: &ProfileStoreFile) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        create_private_dir(parent, "AI config directory")?;
    }
    let bytes = serde_json::to_vec_pretty(store).map_err(|err| err.to_string())?;
    write_private_file(path, &bytes, "AI config file")?;
    if !config_file_is_private(path) {
        let _ = fs::remove_file(path);
        return Err("Failed to restrict the AI config file to the current user".to_string());
    }
    Ok(())
}

pub fn config_file_is_private(path: &Path) -> bool {
    path_is_private(path)
}

fn resolve_provider(
    request: &AgentGenerateRequest,
    base_env: &BTreeMap<String, String>,
) -> Result<(String, Option<String>), String> {
    let llm = request.llm.clone().unwrap_or_default();
    let mut provider = llm
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("auto")
        .to_string();
    if provider == "template" {
        return Err(
            "已禁用离线假模板模式，请配置真实 LLM（OpenAI / Gemini）API Key 或本地兼容端点。"
                .to_string(),
        );
    }
    if !matches!(provider.as_str(), "auto" | "openai" | "gemini") {
        return Err(
            "Invalid 'llm.provider': expected 'auto', 'openai', or 'gemini'".to_string(),
        );
    }
    let api_key = usable_secret(llm.api_key.as_deref());
    let model = llm.model.as_deref().map(str::trim).unwrap_or("");
    let base_url = llm.base_url.as_deref().map(str::trim).unwrap_or("");
    let has_env_openai = env_nonempty(base_env, "OPENAI_API_KEY");
    let has_env_gemini = env_nonempty(base_env, "GEMINI_API_KEY");
    if provider == "auto" {
        if !api_key.as_deref().unwrap_or("").is_empty() || !base_url.is_empty() {
            if base_url
                .to_ascii_lowercase()
                .contains("generativelanguage.googleapis.com")
            {
                provider = "gemini".to_string();
            } else if !base_url.is_empty() {
                provider = "openai".to_string();
            } else {
                let env_provider = base_env
                    .get("CAD_AGENT_PROVIDER")
                    .map(|value| value.trim().eq_ignore_ascii_case("gemini"))
                    .unwrap_or(false);
                let looks_gemini = model.to_ascii_lowercase().contains("gemini")
                    || api_key.as_deref().is_some_and(|key| key.starts_with("AIza"))
                    || (model.is_empty() && env_provider);
                provider = if looks_gemini {
                    "gemini".to_string()
                } else {
                    "openai".to_string()
                };
            }
        } else if has_env_openai {
            provider = "openai".to_string();
        } else if has_env_gemini {
            provider = "gemini".to_string();
        } else {
            return Err(
                "未配置真实 LLM API Key，已禁用离线假模板回退。请配置 OpenAI / Gemini API Key 或本地兼容端点。"
                    .to_string(),
            );
        }
    }
    match provider.as_str() {
        "gemini" => {
            if api_key.is_none() && !has_env_gemini {
                return Err(
                    "未配置 Gemini API Key，请填写密钥或配置环境变量 GEMINI_API_KEY。".to_string(),
                );
            }
        }
        "openai" => {
            if api_key.is_none() && !has_env_openai && base_url.is_empty() {
                return Err(
                    "未配置 OpenAI API Key，请填写密钥、本地兼容 Base URL 或配置环境变量 OPENAI_API_KEY。"
                        .to_string(),
                );
            }
        }
        _ => {}
    }
    Ok((provider, api_key))
}

fn usable_secret(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() || trimmed.contains('•') {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn clean_ids(values: &[String], label: &str) -> Result<Vec<String>, String> {
    if values.len() > 64 {
        return Err(format!("{label} exceeds 64 entries"));
    }
    let mut cleaned = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().count() > 256 || trimmed.chars().any(char::is_control) {
            return Err(format!(
                "{label} entries must be short text without control characters"
            ));
        }
        if !cleaned.iter().any(|existing: &String| existing == trimmed) {
            cleaned.push(trimmed.to_string());
        }
    }
    Ok(cleaned)
}

fn clean_optional_path(value: Option<&str>, label: &str) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > 1024 || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must be short text without control characters"
        ));
    }
    Ok(Some(value.to_string()))
}

fn normalize_lexical(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    let mut has_root = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => {
                has_root = true;
                out.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    if has_root {
                        return Err("E_PATH_ESCAPE: project_path escapes workspace root".to_string());
                    }
                    return Err("E_PATH_ESCAPE: project_path escapes workspace root".to_string());
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Ok(out)
}

fn path_compare_key(path: &Path) -> PathBuf {
    let text = strip_verbatim_prefix(path).to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        PathBuf::from(text.replace('/', "\\").to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(text)
    }
}

fn path_is_inside(root: &Path, path: &Path) -> bool {
    path_compare_key(path).starts_with(&path_compare_key(root))
}

fn resolve_project_in_workspace(workspace_root: &Path, raw: &str) -> Result<String, String> {
    // Emit the caller-supplied workspace spelling. `fs::canonicalize` adds the
    // Windows `\\?\` prefix, and Python `Path.relative_to` then rejects an
    // in-workspace project against the `--workspace-root` argument.
    let norm_root = normalize_lexical(&strip_verbatim_prefix(workspace_root))?;
    let requested = strip_verbatim_prefix(Path::new(raw));
    let candidate = if requested.is_absolute() {
        requested
    } else {
        norm_root.join(requested)
    };
    let norm_candidate = normalize_lexical(&candidate)?;
    if !path_is_inside(&norm_root, &norm_candidate) {
        return Err(format!(
            "E_PATH_ESCAPE: project_path `{raw}` escapes workspace root"
        ));
    }
    if workspace_root.exists() {
        let mut probe = norm_candidate.as_path();
        while !probe.exists() {
            let Some(parent) = probe.parent() else {
                break;
            };
            probe = parent;
        }
        if probe.exists() {
            let canon_probe = fs::canonicalize(probe)
                .map_err(|err| format!("Failed to resolve project_path `{raw}`: {err}"))?;
            let canon_root = fs::canonicalize(workspace_root)
                .map_err(|err| format!("Failed to resolve workspace root: {err}"))?;
            if !path_is_inside(&canon_root, &canon_probe) {
                return Err(format!(
                    "E_PATH_ESCAPE: project_path `{raw}` escapes workspace root via symlink"
                ));
            }
        }
    }
    Ok(path_string(&strip_verbatim_prefix(&norm_candidate)))
}

fn push_selection_args(
    args: &mut Vec<String>,
    help: &str,
    plural: &str,
    singular: &str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    if help.contains(plural) {
        args.push(plural.to_string());
        args.push(serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string()));
        return;
    }
    if help.contains(singular) {
        for value in values {
            args.push(singular.to_string());
            args.push(value.clone());
        }
    }
}

fn collect_secrets(env: &BTreeMap<String, String>, explicit: Option<&str>) -> Vec<String> {
    let mut secrets = Vec::new();
    for key in ["OPENAI_API_KEY", "GEMINI_API_KEY"] {
        if let Some(value) = env.get(key).map(|value| value.trim())
            && value.len() >= 4
            && !secrets.iter().any(|secret| secret == value)
        {
            secrets.push(value.to_string());
        }
    }
    if let Some(value) = explicit.map(str::trim)
        && value.len() >= 4
        && !secrets.iter().any(|secret| secret == value)
    {
        secrets.push(value.to_string());
    }
    secrets
}

fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.len() >= 4 {
            out = out.replace(secret, "***");
        }
    }
    cad_observe::sanitize::redact_secrets(&out)
}

fn redact_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = redact_text(text, secrets),
        Value::Array(items) => {
            for item in items {
                redact_value(item, secrets);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_value(item, secrets);
            }
        }
        _ => {}
    }
}

fn slots() -> &'static Mutex<RunSlots> {
    static SLOTS: OnceLock<Mutex<RunSlots>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        Mutex::new(RunSlots {
            active: HashMap::new(),
            pending_cancel: HashSet::new(),
        })
    })
}

fn lock_slots() -> std::sync::MutexGuard<'static, RunSlots> {
    slots().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register_run(trace_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let mut slots = lock_slots();
    if slots.pending_cancel.remove(trace_id) {
        flag.store(true, Ordering::SeqCst);
    }
    slots.active.insert(trace_id.to_string(), Arc::clone(&flag));
    flag
}

fn finish_run(trace_id: &str) {
    lock_slots().active.remove(trace_id);
}

fn record_line(stream: OutputStream, line: &str, stdout: &mut String, stderr: &mut String) {
    let target = match stream {
        OutputStream::Stdout => stdout,
        OutputStream::Stderr => stderr,
    };
    target.push_str(line);
    target.push('\n');
}

fn read_pipe_lines(
    mut reader: impl Read,
    stream: OutputStream,
    tx: &std::sync::mpsc::Sender<(OutputStream, String)>,
) {
    let mut buffer = [0_u8; 4096];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                pending.extend_from_slice(&buffer[..count]);
                while let Some(position) = pending.iter().position(|byte| *byte == b'\n') {
                    let mut line: Vec<u8> = pending.drain(..=position).collect();
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    let text = String::from_utf8_lossy(&line).into_owned();
                    if tx.send((stream, text)).is_err() {
                        return;
                    }
                }
                if pending.len() > 1024 * 1024 {
                    let text = String::from_utf8_lossy(&pending).into_owned();
                    pending.clear();
                    if tx.send((stream, text)).is_err() {
                        return;
                    }
                }
            }
        }
    }
    if !pending.is_empty() {
        let text = String::from_utf8_lossy(&pending).into_owned();
        let _ = tx.send((stream, text));
    }
}

fn newest_file(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for path in paths {
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let replace = best
            .as_ref()
            .is_none_or(|(previous, _)| modified >= *previous);
        if replace {
            best = Some((modified, path.clone()));
        }
    }
    best.map(|(_, path)| path)
}

fn unique_dir_name(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{nanos}-{sequence}", std::process::id())
}

fn unique_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(unique_dir_name(prefix))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bounded_text(value: Option<&str>, default: &str, max_chars: usize) -> String {
    let text = value.map(str::trim).filter(|text| !text.is_empty()).unwrap_or(default);
    truncate_chars(text, max_chars)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn clip(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 500 {
        trimmed.to_string()
    } else {
        trimmed.chars().take(500).collect()
    }
}

fn key_has_header_break(key: &str) -> bool {
    key.chars().any(|ch| ch == '\r' || ch == '\n' || ch == '\0')
}

fn is_local_endpoint(base_url: &str) -> bool {
    let Some(host) = endpoint_host(base_url) else {
        return false;
    };
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".local")
}

fn endpoint_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(inner) = host_port.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

fn encode_path_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn finish_probe(
    result: Result<ProbeHttp, String>,
    provider: String,
    model: String,
    started: Instant,
    secrets: &[String],
    gemini: bool,
) -> AiConnectionResult {
    let latency_ms = started.elapsed().as_millis();
    match result {
        Ok(response) if (200..300).contains(&response.status) => AiConnectionResult {
            ok: true,
            provider: provider.clone(),
            model: Some(model.clone()),
            status: Some(response.status),
            latency_ms,
            message: if gemini {
                format!("连接成功 · Gemini ({model}) · {latency_ms}ms")
            } else {
                format!("连接成功 · {model} · {latency_ms}ms")
            },
        },
        Ok(response) => {
            let detail = redact_text(&clip(&response.body), secrets);
            let message = if response.status == 401 || response.status == 403 {
                if gemini {
                    format!(
                        "认证失败 (HTTP {})：请检查 Gemini API Key 是否有效。",
                        response.status
                    )
                } else {
                    format!(
                        "认证失败 (HTTP {})：请检查 API Key 权限或余额。",
                        response.status
                    )
                }
            } else if gemini && response.status == 404 {
                format!("模型未找到 (HTTP 404)：请检查模型名称「{model}」。")
            } else if gemini {
                format!(
                    "Gemini 探测失败 (HTTP {}){}",
                    response.status,
                    detail_suffix(&detail)
                )
            } else {
                format!(
                    "端点响应异常 (HTTP {}){}",
                    response.status,
                    detail_suffix(&detail)
                )
            };
            AiConnectionResult {
                ok: false,
                provider,
                model: Some(model),
                status: Some(response.status),
                latency_ms,
                message: redact_text(&message, secrets),
            }
        }
        Err(err) => {
            let detail = redact_text(&err, secrets);
            let timeout = detail.to_ascii_lowercase().contains("timed out")
                || detail.to_ascii_lowercase().contains("timeout")
                || detail.contains("超时");
            AiConnectionResult {
                ok: false,
                provider,
                model: Some(model),
                status: None,
                latency_ms,
                message: if timeout {
                    "连接超时（>12s），请检查 Base URL 或网络代理。".to_string()
                } else {
                    format!("网络请求失败：{detail}")
                },
            }
        }
    }
}

fn connection_failure(
    provider: String,
    model: Option<String>,
    started: Instant,
    message: &str,
    secrets: &[String],
) -> AiConnectionResult {
    AiConnectionResult {
        ok: false,
        provider,
        model,
        status: None,
        latency_ms: started.elapsed().as_millis(),
        message: redact_text(message, secrets),
    }
}

fn detail_suffix(detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        String::new()
    } else {
        format!("：{detail}")
    }
}

fn probe_http_via_python(
    url: &str,
    header_name: &str,
    header_value: &str,
) -> Result<ProbeHttp, String> {
    let python = resolve_python_bin()?;
    let script = r#"
import json, os, sys, urllib.error, urllib.request
url = os.environ["AICAD_PROBE_URL"]
header = os.environ.get("AICAD_PROBE_HEADER", "")
key = os.environ.get("AICAD_PROBE_KEY", "")
headers = {"Accept": "application/json"}
if header and key:
    headers[header] = key
req = urllib.request.Request(url, headers=headers, method="GET")
try:
    with urllib.request.urlopen(req, timeout=12) as resp:
        body = resp.read(2048).decode("utf-8", errors="replace")[:400]
        print(json.dumps({"status": int(resp.status), "body": body}))
except urllib.error.HTTPError as exc:
    try:
        body = exc.read(2048).decode("utf-8", errors="replace")[:400]
    except Exception:
        body = str(exc)
    print(json.dumps({"status": int(exc.code), "body": body}))
except Exception as exc:
    print(json.dumps({"status": 0, "error": str(exc)}))
    sys.exit(1)
"#;
    let output = Command::new(&python)
        .args(["-c", script])
        .env("PYTHONUTF8", "1")
        .env("AICAD_PROBE_URL", url)
        .env("AICAD_PROBE_HEADER", header_name)
        .env("AICAD_PROBE_KEY", header_value)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("failed to launch connectivity probe: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if let Ok(parsed) = serde_json::from_str::<Value>(&stdout) {
        let status = parsed.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
        if let Some(err) = parsed.get("error").and_then(Value::as_str) {
            return Err(clip(err));
        }
        let body = parsed
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok(ProbeHttp { status, body });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(clip(if stderr.trim().is_empty() {
        &stdout
    } else {
        stderr.trim()
    }))
}

fn now_iso() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}



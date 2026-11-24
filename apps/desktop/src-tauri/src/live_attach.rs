//! In-process live-attach host for the desktop app.
//!
//! The Tauri process keeps one [`cad_runtime::DocumentSession`] so the viewport
//! and external agents share the same document. A loopback MCP endpoint on
//! `127.0.0.1` accepts only a 256-bit bearer token and an allow-listed `Origin`.
//! `.aicad-runtime.json` is written when the host starts and removed when it stops.
//! The bearer token is also written to the private token directory so `cad-cli mcp`
//! can authenticate without a pasted secret, and that file is removed with discovery.
//!
//! The session stays on one host thread. `SqliteContainer` owns a SQLite
//! connection that is not `Sync`, so the session is driven exclusively by the
//! host thread while UI commands and accepted sockets are sent over a channel.
//! [`LiveAttachHost`] shares the bearer token.

use std::fs;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cad_mcp::{HttpSecurityConfig, McpServer, bind_streamable_http};
use cad_runtime::{
    Actor, BranchDiff, BranchPurpose, BranchStatus, BranchTarget, CameraPose,
    DISCOVERY_PROTOCOL_VERSION, DocumentRuntime, SemanticCommand, cleanup_runtime_discovery,
    encode_solid_3mf as runtime_encode_solid_3mf, publish_runtime_discovery,
    read_runtime_discovery, resolve_in_workspace,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

/// Studio UI listens for this event and opens Change Preview.
pub const LIVE_ATTACH_PREVIEW_READY_EVENT: &str = "live-attach://preview-ready";

/// Runs on the host thread while the HTTP response is still being finished.
/// Callers must not call back into [`LiveAttachState`] from this hook.
pub type PreviewReadyHook = Arc<dyn Fn(&PendingReview) + Send + Sync>;

const MAX_COMMANDS_JSON_BYTES: usize = 1024 * 1024;
const MAX_SELECTION_ITEMS: usize = 10_000;
const MAX_SELECTION_CHARS: usize = 512;

/// Shared live-attach record. The document session itself lives on the host thread.
pub struct LiveAttachHost {
    token: Option<String>,
}

/// Desktop owner of the host thread and [`LiveAttachHost`].
pub struct LiveAttachState {
    pub host: Arc<Mutex<LiveAttachHost>>,
    tx: Mutex<Option<Sender<Job>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    notify: Arc<PreviewNotify>,
}

/// What `live_attach_start` returns to Studio UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveAttachStarted {
    pub ipc_endpoint: String,
    pub revision: u64,
    pub active_branch: String,
    pub protocol_version: u32,
}

/// Current document revision, preview branches, and viewport selection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveAttachStatus {
    pub revision: u64,
    pub active_branch: String,
    pub pending_preview_branches: Vec<String>,
    pub selected_topo_refs: Vec<String>,
    pub selected_node_ids: Vec<String>,
    pub camera_pose: Option<CameraPose>,
    /// Set when an external agent calls `cad_branch_control` `request_review`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_review: Option<PendingReview>,
}

/// Review payload pushed to Studio UI Change Preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingReview {
    pub branch: String,
    pub revision: u64,
    pub diff: BranchDiff,
}

/// Environment values used to resolve the private bearer-token directory.
///
/// `xdg_runtime_dir` and `home` are read by the Unix resolver. Windows builds
/// still keep them so the same inputs can be constructed on every platform.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub(crate) struct TokenDirInputs {
    pub aicad_token_dir: Option<String>,
    pub local_app_data: Option<String>,
    pub user_profile: Option<String>,
    pub xdg_runtime_dir: Option<String>,
    pub home: Option<String>,
}

/// Revision after a human UI command batch commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveAttachUiResult {
    pub revision: u64,
    pub active_branch: String,
}

struct ActiveAttach {
    server: McpServer,
    security: HttpSecurityConfig,
    workspace_root: PathBuf,
    addr: SocketAddr,
    token_file: PathBuf,
    pending_review: Option<PendingReview>,
}

struct PreviewNotify {
    app: Mutex<Option<AppHandle>>,
    hook: Mutex<Option<PreviewReadyHook>>,
}

struct ToolCall {
    arguments: Value,
}

enum Job {
    Start {
        workspace_root: PathBuf,
        project_path: PathBuf,
        reply: Sender<Result<LiveAttachStarted, String>>,
    },
    Stop {
        reply: Sender<Result<(), String>>,
    },
    UpdateSelection {
        selected_topo_refs: Vec<String>,
        selected_node_ids: Vec<String>,
        reply: Sender<Result<(), String>>,
    },
    Status {
        reply: Sender<Result<LiveAttachStatus, String>>,
    },
    Apply {
        expected_revision: u64,
        commands_json: String,
        reply: Sender<Result<LiveAttachUiResult, String>>,
    },
    Http(TcpStream),
    Exit,
}

impl LiveAttachState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(Mutex::new(LiveAttachHost { token: None }));
        let notify = Arc::new(PreviewNotify {
            app: Mutex::new(None),
            hook: Mutex::new(None),
        });
        let worker_tx = tx.clone();
        let worker_host = Arc::clone(&host);
        let worker_notify = Arc::clone(&notify);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker = thread::Builder::new()
            .name("aicad-live-attach".into())
            .spawn(move || worker_loop(rx, worker_tx, shutdown, worker_host, worker_notify))
            .expect("spawn live attach host thread");
        Self {
            host,
            tx: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
            notify,
        }
    }

    /// Deliver `live-attach://preview-ready` to the Studio UI webview.
    pub fn attach_app(&self, app: AppHandle) {
        *lock_mutex(&self.notify.app) = Some(app);
    }

    /// Test hook for the same payload the Tauri event carries.
    ///
    /// The hook runs on the host thread. It must not call back into this state.
    pub fn set_preview_ready_hook(&self, hook: impl Fn(&PendingReview) + Send + Sync + 'static) {
        *lock_mutex(&self.notify.hook) = Some(Arc::new(hook));
    }

    /// Open or create `project_path` inside `workspace_root` and publish the host.
    pub fn start(
        &self,
        workspace_root: &Path,
        project_path: &Path,
    ) -> Result<LiveAttachStarted, String> {
        self.roundtrip(|reply| Job::Start {
            workspace_root: workspace_root.to_path_buf(),
            project_path: project_path.to_path_buf(),
            reply,
        })
    }

    /// Stop the listener and delete `.aicad-runtime.json`.
    pub fn stop(&self) -> Result<(), String> {
        self.roundtrip(|reply| Job::Stop { reply })
    }

    pub fn bearer_token(&self) -> Result<String, String> {
        lock_mutex(&self.host)
            .token
            .clone()
            .ok_or_else(|| "live attach is not running".to_owned())
    }

    pub fn update_selection(
        &self,
        selected_topo_refs: Vec<String>,
        selected_node_ids: Vec<String>,
    ) -> Result<(), String> {
        let selected_topo_refs = bounded_ids(selected_topo_refs, "selected_topo_refs")?;
        let selected_node_ids = bounded_ids(selected_node_ids, "selected_node_ids")?;
        self.roundtrip(|reply| Job::UpdateSelection {
            selected_topo_refs,
            selected_node_ids,
            reply,
        })
    }

    pub fn status(&self) -> Result<LiveAttachStatus, String> {
        self.roundtrip(|reply| Job::Status { reply })
    }

    /// Apply human UI commands on the active branch if `expected_revision` matches.
    pub fn apply_ui_commands(
        &self,
        expected_revision: u64,
        commands_json: &str,
    ) -> Result<LiveAttachUiResult, String> {
        if commands_json.len() > MAX_COMMANDS_JSON_BYTES {
            return Err("E400_INVALID_COMMAND: commands_json must be at most 1 MiB".into());
        }
        self.roundtrip(|reply| Job::Apply {
            expected_revision,
            commands_json: commands_json.to_owned(),
            reply,
        })
    }

    fn roundtrip<T>(
        &self,
        job: impl FnOnce(Sender<Result<T, String>>) -> Job,
    ) -> Result<T, String> {
        let tx = lock_mutex(&self.tx)
            .as_ref()
            .cloned()
            .ok_or("live attach worker has stopped")?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(job(reply_tx))
            .map_err(|_| "live attach worker has stopped".to_owned())?;
        reply_rx
            .recv()
            .map_err(|_| "live attach worker has stopped".to_owned())?
    }
}

impl Default for LiveAttachState {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LiveAttachState {
    fn drop(&mut self) {
        let tx = lock_mutex(&self.tx).take();
        if let Some(tx) = tx {
            let (reply_tx, reply_rx) = mpsc::channel();
            if tx.send(Job::Stop { reply: reply_tx }).is_ok() {
                let _ = reply_rx.recv_timeout(Duration::from_secs(8));
            }
            let _ = tx.send(Job::Exit);
        }
        if let Some(worker) = lock_mutex(&self.worker).take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(
    rx: Receiver<Job>,
    tx: Sender<Job>,
    shutdown: Arc<AtomicBool>,
    host: Arc<Mutex<LiveAttachHost>>,
    notify: Arc<PreviewNotify>,
) {
    let mut active: Option<ActiveAttach> = None;
    let mut listener: Option<JoinHandle<()>> = None;
    while let Ok(job) = rx.recv() {
        match job {
            Job::Start {
                workspace_root,
                project_path,
                reply,
            } => {
                let result = if active.is_some() {
                    Err("live attach is already running".into())
                } else {
                    match open_attach(&workspace_root, &project_path, &tx, &shutdown) {
                        Ok((started, session, accept_thread)) => {
                            lock_mutex(&host).token = Some(session.security.bearer_token.clone());
                            active = Some(session);
                            listener = Some(accept_thread);
                            Ok(started)
                        }
                        Err(err) => Err(err),
                    }
                };
                let _ = reply.send(result);
            }
            Job::Stop { reply } => {
                let result = stop_attach(&mut active, &mut listener, &shutdown, &host);
                let _ = reply.send(result);
            }
            Job::UpdateSelection {
                selected_topo_refs,
                selected_node_ids,
                reply,
            } => {
                let result = update_selection(&mut active, selected_topo_refs, selected_node_ids);
                let _ = reply.send(result);
            }
            Job::Status { reply } => {
                let _ = reply.send(read_status(&active));
            }
            Job::Apply {
                expected_revision,
                commands_json,
                reply,
            } => {
                let result = apply_ui(&mut active, expected_revision, &commands_json);
                let _ = reply.send(result);
            }
            Job::Http(stream) => {
                if let Some(active) = active.as_mut() {
                    let _ = serve_attach_http(active, stream, &notify);
                }
            }
            Job::Exit => break,
        }
    }
}

pub(crate) fn encode_solid_3mf(
    solid: &cad_kernel_occt::SolidShape,
    model_name: &str,
) -> Result<Vec<u8>, String> {
    runtime_encode_solid_3mf(solid, model_name).map_err(error_text)
}

fn open_attach(
    workspace_root: &Path,
    project_path: &Path,
    tx: &Sender<Job>,
    shutdown: &Arc<AtomicBool>,
) -> Result<(LiveAttachStarted, ActiveAttach, JoinHandle<()>), String> {
    let runtime = DocumentRuntime::live_attached(workspace_root).map_err(error_text)?;
    let project =
        resolve_in_workspace(runtime.workspace_root(), project_path).map_err(error_text)?;
    if let Some(parent) = project.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("E_IO: {err}"))?;
    }
    let session = if project.is_file() {
        runtime.open_session(&project).map_err(error_text)?
    } else {
        let name = project
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .ok_or("E400_INVALID_ARGUMENT: project path needs a file name")?;
        runtime.create_session(&project, name).map_err(error_text)?
    };

    let tcp = bind_streamable_http().map_err(|err| format!("E_IO: {err}"))?;
    let addr = tcp.local_addr().map_err(|err| format!("E_IO: {err}"))?;
    let endpoint = format!("http://127.0.0.1:{}/mcp", addr.port());
    let workspace = session.workspace_root().to_path_buf();
    let project_label = session.path().display().to_string();
    let revision = session.revision();
    let active_branch = session.active_branch().to_owned();
    let published =
        publish_runtime_discovery(&workspace, &endpoint, &project_label).map_err(error_text)?;
    let mut discovery = DiscoveryGuard {
        workspace: workspace.clone(),
        token_file: None,
        armed: true,
    };
    if published.info.protocol_version != DISCOVERY_PROTOCOL_VERSION {
        return Err("E_DISCOVERY: unexpected discovery protocol version".into());
    }
    let written = read_runtime_discovery(&workspace).map_err(error_text)?;
    if written != published.info {
        return Err("E_DISCOVERY: discovery file does not match the published record".into());
    }
    let token = encode_hex(&published.auth_token);
    let token_file = match write_bearer_token(&published.info.auth_token_sha256, &token) {
        Ok(path) => path,
        Err(err) => return Err(err),
    };
    discovery.token_file = Some(token_file.clone());
    let started = LiveAttachStarted {
        ipc_endpoint: endpoint,
        revision,
        active_branch,
        protocol_version: published.info.protocol_version,
    };
    let session = ActiveAttach {
        server: McpServer::from_session(session),
        security: HttpSecurityConfig {
            allowed_origins: loopback_origins(addr.port()),
            bearer_token: token,
        },
        workspace_root: workspace,
        addr,
        token_file,
        pending_review: None,
    };
    shutdown.store(false, Ordering::SeqCst);
    let accept_tx = tx.clone();
    let accept_shutdown = Arc::clone(shutdown);
    let accept_thread = match thread::Builder::new()
        .name("aicad-live-attach-http".into())
        .spawn(move || accept_loop(tcp, accept_tx, accept_shutdown))
    {
        Ok(handle) => handle,
        Err(err) => return Err(format!("E_IO: {err}")),
    };
    discovery.armed = false;
    Ok((started, session, accept_thread))
}

fn stop_attach(
    active: &mut Option<ActiveAttach>,
    listener: &mut Option<JoinHandle<()>>,
    shutdown: &AtomicBool,
    host: &Mutex<LiveAttachHost>,
) -> Result<(), String> {
    shutdown.store(true, Ordering::SeqCst);
    if let Some(active) = active.as_ref() {
        let _ = TcpStream::connect_timeout(&active.addr, Duration::from_secs(2));
    }
    if let Some(listener) = listener.take() {
        let _ = listener.join();
    }
    let finished = active.take();
    lock_mutex(host).token = None;
    shutdown.store(false, Ordering::SeqCst);
    let Some(finished) = finished else {
        return Ok(());
    };
    let discovery_result = cleanup_runtime_discovery(&finished.workspace_root).map_err(error_text);
    let token_result = remove_token_file(&finished.token_file);
    match (discovery_result, token_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), Ok(())) | (Ok(()), Err(err)) => Err(err),
        (Err(discovery), Err(token)) => Err(format!("{discovery}; {token}")),
    }
}

fn update_selection(
    active: &mut Option<ActiveAttach>,
    selected_topo_refs: Vec<String>,
    selected_node_ids: Vec<String>,
) -> Result<(), String> {
    let active = active
        .as_mut()
        .ok_or_else(|| "live attach is not running".to_owned())?;
    let mut viewport = active.server.session().viewport().clone();
    viewport.selected_topo_refs = selected_topo_refs;
    viewport.selected_node_ids = selected_node_ids;
    active.server.session_mut().set_viewport(viewport);
    Ok(())
}

fn read_status(active: &Option<ActiveAttach>) -> Result<LiveAttachStatus, String> {
    let active = active
        .as_ref()
        .ok_or_else(|| "live attach is not running".to_owned())?;
    let session = active.server.session();
    let viewport = session.viewport().clone();
    Ok(LiveAttachStatus {
        revision: session.revision(),
        active_branch: session.active_branch().to_owned(),
        pending_preview_branches: pending_previews(session)?,
        selected_topo_refs: viewport.selected_topo_refs,
        selected_node_ids: viewport.selected_node_ids,
        camera_pose: viewport.camera_pose,
        pending_review: active.pending_review.clone(),
    })
}

fn apply_ui(
    active: &mut Option<ActiveAttach>,
    expected_revision: u64,
    commands_json: &str,
) -> Result<LiveAttachUiResult, String> {
    let commands: Vec<SemanticCommand> = serde_json::from_str(commands_json)
        .map_err(|err| format!("E400_INVALID_COMMAND: {err}"))?;
    if commands.is_empty() {
        return Err(
            "E400_INVALID_COMMAND: commands must contain at least one SemanticCommand".into(),
        );
    }
    let active = active
        .as_mut()
        .ok_or_else(|| "live attach is not running".to_owned())?;
    match active.server.session_mut().exec_semantic_cmds(
        Some(expected_revision),
        Actor::HumanUi,
        BranchTarget::Active,
        commands,
    ) {
        Ok(outcome) => Ok(LiveAttachUiResult {
            revision: outcome.revision,
            active_branch: outcome.branch,
        }),
        Err(err) => Err(error_text(err)),
    }
}

fn accept_loop(listener: TcpListener, tx: Sender<Job>, shutdown: Arc<AtomicBool>) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if tx.send(Job::Http(stream)).is_err() {
                    break;
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

struct DiscoveryGuard {
    workspace: PathBuf,
    token_file: Option<PathBuf>,
    armed: bool,
}

impl Drop for DiscoveryGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = cleanup_runtime_discovery(&self.workspace);
            if let Some(path) = self.token_file.take() {
                let _ = remove_token_file(&path);
            }
        }
    }
}

fn serve_attach_http(
    active: &mut ActiveAttach,
    mut stream: TcpStream,
    notify: &PreviewNotify,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut review_to_emit = None;
    active.security.serve_one_http_connection_with_hook(
        &mut stream,
        &mut active.server,
        |server, body, response| {
            if let Some(call) = tool_call_of_body(body).filter(is_request_review) {
                review_to_emit = review_after_request(server, &call, response);
            }
        },
    )?;
    if let Some(review) = review_to_emit {
        active.pending_review = Some(review.clone());
        notify.emit(&review);
    }
    Ok(())
}

fn review_after_request(
    server: &mut McpServer,
    call: &ToolCall,
    response: &str,
) -> Option<PendingReview> {
    if !response.starts_with("HTTP/1.1 200") {
        return None;
    }
    let revision = server.session().revision();
    match review_from_response(response, revision) {
        ReviewParse::Ready(review) => Some(review),
        ReviewParse::Failed => None,
        ReviewParse::Unparsed => {
            let branch = call
                .arguments
                .get("branch")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| server.session().active_branch().to_owned());
            let diff = server.session_mut().diff_branch(&branch).ok()?;
            let revision = server.session().revision();
            Some(PendingReview {
                branch,
                revision,
                diff,
            })
        }
    }
}

enum ReviewParse {
    Ready(PendingReview),
    Failed,
    Unparsed,
}

fn review_from_response(response: &str, revision: u64) -> ReviewParse {
    let Some(body) = http_body(response)
        .map(str::trim)
        .filter(|body| !body.is_empty())
    else {
        return ReviewParse::Unparsed;
    };
    let Ok(envelope) = serde_json::from_str::<Value>(body) else {
        return ReviewParse::Unparsed;
    };
    if envelope.get("error").is_some() {
        return ReviewParse::Failed;
    }
    let Some(result) = envelope.get("result") else {
        return ReviewParse::Unparsed;
    };
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ReviewParse::Failed;
    }
    let Some(text) = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
    else {
        return ReviewParse::Unparsed;
    };
    let Ok(payload) = serde_json::from_str::<Value>(text) else {
        return ReviewParse::Unparsed;
    };
    if payload.get("action").and_then(Value::as_str) != Some("request_review") {
        return ReviewParse::Failed;
    }
    let Some(branch) = payload
        .get("branch")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|branch| !branch.is_empty())
    else {
        return ReviewParse::Unparsed;
    };
    let Some(diff) = payload
        .get("diff")
        .cloned()
        .and_then(|value| serde_json::from_value::<BranchDiff>(value).ok())
    else {
        return ReviewParse::Unparsed;
    };
    ReviewParse::Ready(PendingReview {
        branch,
        revision,
        diff,
    })
}

fn tool_call_of_body(body: &str) -> Option<ToolCall> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(body).ok()?;
    if value.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    let params = value.get("params")?;
    let name = params.get("name").and_then(Value::as_str)?;
    if name != "cad_branch_control" {
        return None;
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ToolCall { arguments })
}

fn is_request_review(call: &ToolCall) -> bool {
    call.arguments.get("action").and_then(Value::as_str) == Some("request_review")
}

fn http_body(raw: &str) -> Option<&str> {
    raw.find("\r\n\r\n")
        .map(|index| &raw[index + 4..])
        .or_else(|| raw.find("\n\n").map(|index| &raw[index + 2..]))
}

impl PreviewNotify {
    fn emit(&self, review: &PendingReview) {
        if let Some(hook) = lock_mutex(&self.hook).clone() {
            hook(review);
        }
        if let Some(app) = lock_mutex(&self.app).clone() {
            if let Err(err) = app.emit(LIVE_ATTACH_PREVIEW_READY_EVENT, review) {
                eprintln!("live-attach preview event was not delivered: {err}");
            }
        }
    }
}

/// Directory that holds `<sha256 prefix>.token` files.
pub(crate) fn resolve_token_dir(inputs: &TokenDirInputs) -> Result<PathBuf, String> {
    if let Some(dir) = nonempty(inputs.aicad_token_dir.as_deref()) {
        return Ok(PathBuf::from(dir));
    }
    platform_token_dir(inputs)
}

pub(crate) fn current_token_dir() -> Result<PathBuf, String> {
    resolve_token_dir(&TokenDirInputs {
        aicad_token_dir: std::env::var("AICAD_TOKEN_DIR").ok(),
        local_app_data: std::env::var("LOCALAPPDATA").ok(),
        user_profile: std::env::var("USERPROFILE").ok(),
        xdg_runtime_dir: std::env::var("XDG_RUNTIME_DIR").ok(),
        home: std::env::var("HOME").ok(),
    })
}

pub(crate) fn token_file_path(dir: &Path, auth_token_sha256: &str) -> Result<PathBuf, String> {
    let prefix: String = auth_token_sha256.chars().take(16).collect();
    if prefix.chars().count() != 16 || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("E_DISCOVERY: auth_token_sha256 prefix must be 16 hex characters".to_owned());
    }
    Ok(dir.join(format!("{prefix}.token")))
}

fn write_bearer_token(auth_token_sha256: &str, token_hex: &str) -> Result<PathBuf, String> {
    if token_hex.len() != 64 || !token_hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("E_IO: bearer token must be 64 hex characters".into());
    }
    let dir = current_token_dir()?;
    create_token_dir(&dir)?;
    let path = token_file_path(&dir, auth_token_sha256)?;
    if let Err(err) = write_token_file(&path, token_hex) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(path)
}

fn remove_token_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("E_IO: {err}")),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

#[cfg(windows)]
fn platform_token_dir(inputs: &TokenDirInputs) -> Result<PathBuf, String> {
    if let Some(local) = nonempty(inputs.local_app_data.as_deref()) {
        return Ok(PathBuf::from(local).join("aicad").join("tokens"));
    }
    if let Some(profile) = nonempty(inputs.user_profile.as_deref()) {
        return Ok(PathBuf::from(profile).join(".aicad").join("tokens"));
    }
    Err("E_IO: token directory is unset. Set AICAD_TOKEN_DIR or LOCALAPPDATA.".into())
}

#[cfg(not(windows))]
fn platform_token_dir(inputs: &TokenDirInputs) -> Result<PathBuf, String> {
    if let Some(runtime) = nonempty(inputs.xdg_runtime_dir.as_deref()) {
        return Ok(PathBuf::from(runtime).join("aicad").join("tokens"));
    }
    if let Some(home) = nonempty(inputs.home.as_deref()) {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("aicad")
            .join("tokens"));
    }
    Err("E_IO: token directory is unset. Set AICAD_TOKEN_DIR, XDG_RUNTIME_DIR, or HOME.".into())
}

/// Return `true` when `dir` falls under a shared/public Windows directory that
/// should never hold a per-user bearer token (e.g. `C:\Users\Public`,
/// `C:\ProgramData`, `C:\Users\Default`, `C:\Windows`, or caller-supplied roots).
pub fn is_shared_public_token_dir(dir: &Path, extra_shared_roots: &[&str]) -> bool {
    let normalized = normalize_windows_like_path(dir);
    let mut roots: Vec<String> = vec![
        r"c:\users\public".to_owned(),
        r"c:\users\default".to_owned(),
        r"c:\programdata".to_owned(),
        r"c:\windows".to_owned(),
    ];
    for key in ["PUBLIC", "PROGRAMDATA", "ALLUSERSPROFILE", "WINDIR", "SYSTEMROOT"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                roots.push(normalize_windows_like_path(Path::new(trimmed)));
            }
        }
    }
    for root in extra_shared_roots {
        let trimmed = root.trim();
        if !trimmed.is_empty() {
            roots.push(normalize_windows_like_path(Path::new(trimmed)));
        }
    }
    roots.iter().any(|root| windows_path_within(&normalized, root))
}

fn normalize_windows_like_path(path: &Path) -> String {
    let stripped = crate::private_fs::strip_verbatim_prefix(path)
        .to_string_lossy()
        .into_owned();
    let unified = stripped.replace('/', "\\").to_ascii_lowercase();
    let (prefix, rest) = if let Some(after_unc) = unified.strip_prefix(r"\\") {
        let mut parts = after_unc.splitn(3, '\\');
        let host = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        (format!(r"\\{host}\{share}"), tail)
    } else if unified.len() >= 2 && unified.as_bytes()[1] == b':' {
        (unified[..2].to_owned(), unified[2..].trim_start_matches('\\'))
    } else {
        (String::new(), unified.as_str())
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in rest.split('\\') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    if prefix.is_empty() {
        stack.join("\\")
    } else if stack.is_empty() {
        format!("{prefix}\\")
    } else {
        format!("{prefix}\\{}", stack.join("\\"))
    }
}

fn windows_path_within(candidate: &str, root: &str) -> bool {
    if candidate == root {
        return true;
    }
    let Some(rest) = candidate.strip_prefix(root) else {
        return false;
    };
    rest.starts_with('\\')
}

fn create_token_dir(dir: &Path) -> Result<(), String> {
    if is_shared_public_token_dir(dir, &[]) {
        return Err(format!(
            "E_IO: refusing to store bearer token in shared public directory `{}`",
            dir.display()
        ));
    }
    crate::private_fs::create_private_dir(dir, "token directory")
}

fn write_token_file(path: &Path, token_hex: &str) -> Result<(), String> {
    crate::private_fs::write_private_file(path, token_hex.as_bytes(), "token file")?;
    if !crate::private_fs::path_is_private(path) {
        let _ = fs::remove_file(path);
        return Err(format!(
            "E_IO: failed to restrict bearer token file `{}` to the current user",
            path.display()
        ));
    }
    Ok(())
}

fn pending_previews(session: &cad_runtime::DocumentSession) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for branch in session.list_branches().map_err(error_text)? {
        let preview_name =
            branch.branch_name.starts_with("agent/") || branch.branch_name.starts_with("preview/");
        if branch.branch_name != "main"
            && branch.status == BranchStatus::Active
            && (branch.purpose == BranchPurpose::AiCandidate || preview_name)
        {
            names.push(branch.branch_name);
        }
    }
    names.sort();
    Ok(names)
}

fn bounded_ids(ids: Vec<String>, label: &str) -> Result<Vec<String>, String> {
    if ids.len() > MAX_SELECTION_ITEMS {
        return Err(format!(
            "E400_INVALID_ARGUMENT: {label} exceeds {MAX_SELECTION_ITEMS} entries"
        ));
    }
    if ids
        .iter()
        .any(|id| id.len() > MAX_SELECTION_CHARS || id.chars().any(char::is_control))
    {
        return Err(format!(
            "E400_INVALID_ARGUMENT: {label} entries must be short text without control characters"
        ));
    }
    Ok(ids)
}

fn loopback_origins(port: u16) -> Vec<String> {
    vec![
        "http://127.0.0.1".to_owned(),
        format!("http://127.0.0.1:{port}"),
        "http://localhost".to_owned(),
        format!("http://localhost:{port}"),
    ]
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn error_text(err: cad_runtime::RuntimeError) -> String {
    let code = err.code();
    let text = err.to_string();
    if text.contains(code) {
        text
    } else {
        format!("{code}: {text}")
    }
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tauri::command]
pub fn live_attach_start(
    workspace_root: String,
    project_path: String,
    host: State<'_, LiveAttachState>,
) -> Result<LiveAttachStarted, String> {
    host.start(Path::new(&workspace_root), Path::new(&project_path))
}

#[tauri::command]
pub fn live_attach_stop(host: State<'_, LiveAttachState>) -> Result<(), String> {
    host.stop()
}

#[tauri::command]
pub fn live_attach_update_selection(
    selected_topo_refs: Vec<String>,
    selected_node_ids: Vec<String>,
    host: State<'_, LiveAttachState>,
) -> Result<(), String> {
    host.update_selection(selected_topo_refs, selected_node_ids)
}

#[tauri::command]
pub fn live_attach_get_status(
    host: State<'_, LiveAttachState>,
) -> Result<LiveAttachStatus, String> {
    host.status()
}

pub(crate) struct PreviewBranchActionOutcome {
    pub active_branch: String,
    pub diff: Option<serde_json::Value>,
    pub revision: u64,
}

pub(crate) fn preview_branch_action(
    path: &Path,
    workspace_root: &Path,
    action: &str,
    branch_name: &str,
) -> Result<PreviewBranchActionOutcome, String> {
    match action {
        "diff" => {
            let mut session = cad_runtime::DocumentSession::open(path, workspace_root)
                .map_err(|err| format!("cannot open runtime session: {err}"))?;
            let diff = session
                .diff_branch(branch_name)
                .map_err(|err| format!("cannot diff branch `{branch_name}`: {err}"))?;
            let diff_value = serde_json::to_value(&diff)
                .map_err(|err| format!("cannot serialize branch diff: {err}"))?;
            Ok(PreviewBranchActionOutcome {
                active_branch: session.active_branch().to_owned(),
                diff: Some(diff_value),
                revision: session.revision(),
            })
        }
        "commit_preview" => {
            let (revision, needs_main_checkout) = {
                let mut session = cad_runtime::DocumentSession::open(path, workspace_root)
                    .map_err(|err| format!("cannot open runtime session: {err}"))?;
                let outcome = session
                    .commit_preview(branch_name)
                    .map_err(|err| format!("cannot commit preview `{branch_name}`: {err}"))?;
                (outcome.revision, session.active_branch() != "main")
            };
            if needs_main_checkout {
                let mut container = cad_container::SqliteContainer::open(path)
                    .map_err(|err| format!("cannot open SQLite project: {err}"))?;
                container
                    .checkout_branch("main")
                    .map_err(|err| format!("cannot checkout main after commit_preview: {err}"))?;
            }
            Ok(PreviewBranchActionOutcome {
                active_branch: "main".to_owned(),
                diff: None,
                revision,
            })
        }
        "discard_preview" => {
            let mut session = cad_runtime::DocumentSession::open(path, workspace_root)
                .map_err(|err| format!("cannot open runtime session: {err}"))?;
            session
                .discard_preview(branch_name)
                .map_err(|err| format!("cannot discard preview `{branch_name}`: {err}"))?;
            Ok(PreviewBranchActionOutcome {
                active_branch: session.active_branch().to_owned(),
                diff: None,
                revision: session.revision(),
            })
        }
        other => Err(format!("Unsupported preview branch action `{other}`")),
    }
}

#[tauri::command]
pub fn live_attach_apply_ui_commands(
    expected_revision: u64,
    commands_json: String,
    host: State<'_, LiveAttachState>,
) -> Result<LiveAttachUiResult, String> {
    host.apply_ui_commands(expected_revision, &commands_json)
}

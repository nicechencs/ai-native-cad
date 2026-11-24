use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 4-byte little-endian magic header (`"CGMD"`) for framed `cad-geomd` IPC messages.
pub const GEOMD_PROTOCOL_MAGIC: u32 = 0x4347_4D44;

/// Maximum permitted framed JSON payload size (64 MiB), matching `CAD_GEOMD_MAX_FRAME_BYTES`.
pub const GEOMD_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Environment variable overriding the `cad-geomd` binary path.
pub const CAD_GEOMD_BIN_ENV: &str = "CAD_GEOMD_BIN";

/// Structured error returned by [`GeomdClient`] and framed IPC helpers.
#[derive(Debug, thiserror::Error)]
pub enum GeomdError {
    #[error("failed to spawn cad-geomd binary at '{binary}': {source}")]
    SpawnFailed {
        binary: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("framed IPC protocol error: {0}")]
    Protocol(String),
    #[error("JSON serialization/deserialization error: {0}")]
    Serialization(String),
    #[error("I/O error communicating with cad-geomd: {0}")]
    Io(#[from] io::Error),
    #[error("RPC '{method}' timed out after {timeout:?}; daemon process terminated for isolation")]
    Timeout { method: String, timeout: Duration },
    #[error("cad-geomd exited unexpectedly during '{method}' (exit_code={exit_code:?}): {detail}")]
    ProcessCrashed {
        method: String,
        exit_code: Option<i32>,
        detail: String,
    },
    #[error("cad-geomd remote error [{name} ({code})]: {message}")]
    Remote {
        code: i32,
        name: String,
        message: String,
    },
    #[error("invalid response from cad-geomd: {0}")]
    InvalidResponse(String),
    #[error("cad-geomd client session is already closed")]
    Closed,
}

/// Kernel backend selection passed via `cad-geomd --kernel <auto|analytic|occt>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeomdKernelPreference {
    Auto,
    Analytic,
    Occt,
}

impl GeomdKernelPreference {
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Analytic => "analytic",
            Self::Occt => "occt",
        }
    }
}

/// Configuration for spawning a process-isolated [`GeomdClient`].
#[derive(Debug, Clone)]
pub struct GeomdClientConfig {
    pub binary_path: PathBuf,
    pub workers: Option<u32>,
    pub kernel: Option<GeomdKernelPreference>,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
}

impl GeomdClientConfig {
    pub fn new(binary_path: impl Into<PathBuf>) -> Self {
        Self {
            binary_path: binary_path.into(),
            workers: None,
            kernel: None,
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_workers(mut self, workers: u32) -> Self {
        self.workers = Some(workers);
        self
    }

    pub fn with_kernel(mut self, kernel: GeomdKernelPreference) -> Self {
        self.kernel = Some(kernel);
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }
}

/// Boolean operation kind for `cad-geomd` `boolean` RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeomdBooleanOp {
    Union,
    Cut,
    Intersect,
}

impl GeomdBooleanOp {
    pub fn as_rpc_str(self) -> &'static str {
        match self {
            Self::Union => "union",
            Self::Cut => "cut",
            Self::Intersect => "intersect",
        }
    }
}

/// Tessellation parameters for `cad-geomd` `tessellate` RPC.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeomdTessellationParams {
    pub lod_level: u8,
    pub linear_deflection_mm: f64,
    pub angular_deflection_rad: f64,
    pub cylinder_segments: u32,
}

impl Default for GeomdTessellationParams {
    fn default() -> Self {
        Self {
            lod_level: 1,
            linear_deflection_mm: 0.05,
            angular_deflection_rad: 0.2,
            cylinder_segments: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeomdPingResponse {
    pub pong: bool,
    pub abi_major: u32,
    pub abi_minor: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeomdCapabilities {
    pub kernel_name: String,
    pub kernel_version: String,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub authoritative_brep: bool,
    pub supports_parallel_tessellation: bool,
    pub supports_step_ap242: bool,
    pub supports_exact_booleans: bool,
    pub supports_two_stage_assembly: bool,
    pub worker_threads: u32,
    #[serde(default)]
    pub occt_compiled: bool,
    #[serde(default)]
    pub available_kernels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeomdShapeResponse {
    pub handle: u64,
    pub volume_mm3: f64,
    pub face_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeomdMeshResponse {
    pub handle: u64,
    pub vertex_count: u32,
    pub triangle_count: u32,
    pub face_count: u32,
    pub positions_mm: Vec<f64>,
    pub normals: Vec<f64>,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeomdShutdownResponse {
    pub shutting_down: bool,
}

#[derive(Debug, Deserialize)]
struct RpcErrorPayload {
    code: i32,
    name: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct RpcResponseEnvelope {
    ok: bool,
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErrorPayload>,
}

/// Encode a UTF-8 JSON payload into a framed `cad-geomd` binary message:
/// `[4B magic "CGMD" LE][4B payload_len LE][payload bytes]`.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, GeomdError> {
    if payload.len() > GEOMD_MAX_FRAME_BYTES {
        return Err(GeomdError::Protocol(format!(
            "outgoing frame payload ({} bytes) exceeds maximum ({} bytes)",
            payload.len(),
            GEOMD_MAX_FRAME_BYTES
        )));
    }
    let len = u32::try_from(payload.len())
        .map_err(|_| GeomdError::Protocol("outgoing frame payload exceeds u32 length".into()))?;
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&GEOMD_PROTOCOL_MAGIC.to_le_bytes());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Decode one framed `cad-geomd` binary message from `bytes`, returning `(payload, consumed_bytes)`.
pub fn decode_frame(bytes: &[u8]) -> Result<(Vec<u8>, usize), GeomdError> {
    if bytes.len() < 8 {
        return Err(GeomdError::Protocol("incomplete frame header".into()));
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic != GEOMD_PROTOCOL_MAGIC {
        return Err(GeomdError::Protocol(format!(
            "invalid frame magic: expected 0x{GEOMD_PROTOCOL_MAGIC:08X}, got 0x{magic:08X}"
        )));
    }
    let payload_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if payload_len > GEOMD_MAX_FRAME_BYTES {
        return Err(GeomdError::Protocol(format!(
            "frame payload ({payload_len} bytes) exceeds maximum ({GEOMD_MAX_FRAME_BYTES} bytes)"
        )));
    }
    let total = 8usize
        .checked_add(payload_len)
        .ok_or_else(|| GeomdError::Protocol("frame length overflow".into()))?;
    if bytes.len() < total {
        return Err(GeomdError::Protocol("truncated frame payload".into()));
    }
    Ok((bytes[8..total].to_vec(), total))
}

/// Write a framed binary message to `writer` and flush.
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), GeomdError> {
    let frame = encode_frame(payload)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Read one framed binary message from `reader`.
/// Returns `Ok(None)` on clean EOF before the header begins.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, GeomdError> {
    let mut header = [0u8; 8];
    let mut read_total = 0usize;
    while read_total < header.len() {
        match reader.read(&mut header[read_total..]) {
            Ok(0) => {
                if read_total == 0 {
                    return Ok(None);
                }
                return Err(GeomdError::Protocol(
                    "truncated frame header on stream".into(),
                ));
            }
            Ok(n) => read_total += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(GeomdError::Io(err)),
        }
    }

    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if magic != GEOMD_PROTOCOL_MAGIC {
        return Err(GeomdError::Protocol(format!(
            "invalid frame magic on stream: expected 0x{GEOMD_PROTOCOL_MAGIC:08X}, got 0x{magic:08X}"
        )));
    }
    let payload_len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if payload_len > GEOMD_MAX_FRAME_BYTES {
        return Err(GeomdError::Protocol(format!(
            "incoming frame ({payload_len} bytes) exceeds maximum ({GEOMD_MAX_FRAME_BYTES} bytes)"
        )));
    }

    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        reader.read_exact(&mut payload).map_err(|err| {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                GeomdError::Protocol("truncated frame payload on stream".into())
            } else {
                GeomdError::Io(err)
            }
        })?;
    }
    Ok(Some(payload))
}

/// Auto-locate the built `cad-geomd` executable via `CAD_GEOMD_BIN` or standard
/// `cpp/cad-geometry-engine/build/...` output paths relative to the workspace.
pub fn locate_geomd_binary() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var(CAD_GEOMD_BIN_ENV) {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }

    let exe_name = if cfg!(windows) {
        "cad-geomd.exe"
    } else {
        "cad-geomd"
    };

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest_dir);

    let build_roots = [
        workspace_root.join("cpp/cad-geometry-engine/build"),
        workspace_root.join("cpp/cad-geometry-engine/build-release"),
        workspace_root.join("cpp/cad-geometry-engine/build-debug"),
    ];
    let subdirs = ["Release", "RelWithDebInfo", "Debug", ""];

    for root in &build_roots {
        for sub in &subdirs {
            let candidate = if sub.is_empty() {
                root.join(exe_name)
            } else {
                root.join(sub).join(exe_name)
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

enum ReaderEvent {
    Frame(Vec<u8>),
    Eof,
    Error(GeomdError),
}

/// Process-isolated IPC client communicating with a `cad-geomd --stdio` child process
/// over framed length-prefixed binary JSON-RPC.
pub struct GeomdClient {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<ReaderEvent>,
    reader_thread: Option<JoinHandle<()>>,
    next_id: u64,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    closed: bool,
}

impl GeomdClient {
    /// Spawn a new `cad-geomd --stdio` child process with the provided configuration.
    pub fn spawn(config: GeomdClientConfig) -> Result<Self, GeomdError> {
        let mut cmd = Command::new(&config.binary_path);
        cmd.arg("--stdio");
        if let Some(workers) = config.workers {
            cmd.arg("--workers").arg(workers.to_string());
        }
        if let Some(kernel) = config.kernel {
            cmd.arg("--kernel").arg(kernel.as_cli_arg());
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().map_err(|source| GeomdError::SpawnFailed {
            binary: config.binary_path.clone(),
            source,
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            GeomdError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "failed to attach child stdin",
            ))
        })?;
        let mut stdout = child.stdout.take().ok_or_else(|| {
            GeomdError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "failed to attach child stdout",
            ))
        })?;

        let (tx, rx) = mpsc::channel();
        let reader_thread = thread::Builder::new()
            .name("cad-geomd-stdout-reader".into())
            .spawn(move || loop {
                match read_frame(&mut stdout) {
                    Ok(Some(payload)) => {
                        if tx.send(ReaderEvent::Frame(payload)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(ReaderEvent::Eof);
                        break;
                    }
                    Err(err) => {
                        let _ = tx.send(ReaderEvent::Error(err));
                        break;
                    }
                }
            })
            .map_err(GeomdError::Io)?;

        Ok(Self {
            child,
            stdin: Some(stdin),
            rx,
            reader_thread: Some(reader_thread),
            next_id: 1,
            request_timeout: config.request_timeout,
            shutdown_timeout: config.shutdown_timeout,
            closed: false,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn ping(&mut self) -> Result<GeomdPingResponse, GeomdError> {
        let val = self.call_raw("ping", None)?;
        serde_json::from_value(val).map_err(|e| GeomdError::InvalidResponse(e.to_string()))
    }

    pub fn capabilities(&mut self) -> Result<GeomdCapabilities, GeomdError> {
        let val = self.call_raw("capabilities", None)?;
        serde_json::from_value(val).map_err(|e| GeomdError::InvalidResponse(e.to_string()))
    }

    pub fn create_box(
        &mut self,
        dx: f64,
        dy: f64,
        dz: f64,
        origin: [f64; 3],
    ) -> Result<GeomdShapeResponse, GeomdError> {
        let params = json!({
            "dx": dx,
            "dy": dy,
            "dz": dz,
            "ox": origin[0],
            "oy": origin[1],
            "oz": origin[2],
        });
        let val = self.call_raw("create_box", Some(params))?;
        serde_json::from_value(val).map_err(|e| GeomdError::InvalidResponse(e.to_string()))
    }

    pub fn boolean(
        &mut self,
        base_handle: u64,
        tool_handle: u64,
        op: GeomdBooleanOp,
    ) -> Result<GeomdShapeResponse, GeomdError> {
        let params = json!({
            "base_handle": base_handle,
            "tool_handle": tool_handle,
            "op": op.as_rpc_str(),
        });
        let val = self.call_raw("boolean", Some(params))?;
        serde_json::from_value(val).map_err(|e| GeomdError::InvalidResponse(e.to_string()))
    }

    pub fn tessellate(
        &mut self,
        handle: u64,
        params: &GeomdTessellationParams,
    ) -> Result<GeomdMeshResponse, GeomdError> {
        let rpc_params = json!({
            "handle": handle,
            "lod_level": params.lod_level,
            "linear_deflection_mm": params.linear_deflection_mm,
            "angular_deflection_rad": params.angular_deflection_rad,
            "cylinder_segments": params.cylinder_segments,
        });
        let val = self.call_raw("tessellate", Some(rpc_params))?;
        serde_json::from_value(val).map_err(|e| GeomdError::InvalidResponse(e.to_string()))
    }

    pub fn shutdown(&mut self) -> Result<GeomdShutdownResponse, GeomdError> {
        let val = self.call_raw("shutdown", None)?;
        let resp: GeomdShutdownResponse =
            serde_json::from_value(val).map_err(|e| GeomdError::InvalidResponse(e.to_string()))?;
        self.stdin.take();
        self.closed = true;
        self.wait_or_kill(self.shutdown_timeout);
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        Ok(resp)
    }

    /// Immediately terminate the isolated daemon process and release handles.
    pub fn kill(&mut self) {
        self.closed = true;
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }

    fn call_raw(&mut self, method: &str, params: Option<Value>) -> Result<Value, GeomdError> {
        if self.closed {
            return Err(GeomdError::Closed);
        }

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let request = match params {
            Some(p) => json!({ "id": id, "method": method, "params": p }),
            None => json!({ "id": id, "method": method }),
        };
        let payload = serde_json::to_vec(&request)
            .map_err(|e| GeomdError::Serialization(e.to_string()))?;

        let write_res = match self.stdin.as_mut() {
            Some(stdin) => write_frame(stdin, &payload),
            None => return Err(GeomdError::Closed),
        };
        if let Err(err) = write_res {
            let exit_code = self.collect_exit_code_short();
            self.closed = true;
            return Err(GeomdError::ProcessCrashed {
                method: method.to_owned(),
                exit_code,
                detail: err.to_string(),
            });
        }

        match self.rx.recv_timeout(self.request_timeout) {
            Ok(ReaderEvent::Frame(resp_bytes)) => {
                let envelope: RpcResponseEnvelope = serde_json::from_slice(&resp_bytes)
                    .map_err(|e| GeomdError::Serialization(e.to_string()))?;
                if envelope.id != json!(id) {
                    return Err(GeomdError::InvalidResponse(format!(
                        "mismatched response id: expected {id}, got {}",
                        envelope.id
                    )));
                }
                if envelope.ok {
                    envelope.result.ok_or_else(|| {
                        GeomdError::InvalidResponse("missing 'result' in ok response".into())
                    })
                } else if let Some(err) = envelope.error {
                    Err(GeomdError::Remote {
                        code: err.code,
                        name: err.name,
                        message: err.message,
                    })
                } else {
                    Err(GeomdError::InvalidResponse(
                        "error response missing 'error' payload".into(),
                    ))
                }
            }
            Ok(ReaderEvent::Eof) | Err(RecvTimeoutError::Disconnected) => {
                let exit_code = self.collect_exit_code_short();
                self.closed = true;
                Err(GeomdError::ProcessCrashed {
                    method: method.to_owned(),
                    exit_code,
                    detail: "stdout closed before response frame arrived".into(),
                })
            }
            Ok(ReaderEvent::Error(err)) => {
                let exit_code = self.try_exit_code();
                if exit_code.is_some() {
                    self.closed = true;
                    Err(GeomdError::ProcessCrashed {
                        method: method.to_owned(),
                        exit_code,
                        detail: err.to_string(),
                    })
                } else {
                    Err(err)
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                Err(GeomdError::Timeout {
                    method: method.to_owned(),
                    timeout: self.request_timeout,
                })
            }
        }
    }

    fn try_exit_code(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().and_then(|s| s.code())
    }

    fn collect_exit_code_short(&mut self) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code();
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_or_kill(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        return;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    }
}

impl Drop for GeomdClient {
    fn drop(&mut self) {
        self.stdin.take();
        if !self.closed {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

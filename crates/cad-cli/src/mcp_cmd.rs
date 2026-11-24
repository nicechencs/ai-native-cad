//! `cad-cli mcp`: stdio and loopback HTTP, plus OpenAI tool-schema export.
//!
//! Stdio attaches to a desktop Live-Attach host when `.aicad-runtime.json`
//! points at a live loopback session. `--headless` keeps the in-process runtime.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::ExitCode;

use cad_mcp::{HttpSecurityConfig, McpServer, serve_stdio};
use clap::{Args, ValueEnum};

use crate::runtime_host::{self, HostError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum McpTransport {
    #[default]
    Stdio,
    Http,
}

#[derive(Args)]
pub(crate) struct McpArgs {
    /// `stdio` (default) or loopback `http`.
    #[arg(long, value_enum, default_value_t = McpTransport::Stdio)]
    transport: McpTransport,
    /// `.aicad` project to open or create. Not used with `--export-tool-schema`.
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    /// Workspace root. Relative project paths stay inside this directory.
    #[arg(long, value_name = "DIR", default_value = ".")]
    workspace: PathBuf,
    /// Stay in this process even when a desktop Live-Attach host is running.
    #[arg(long)]
    headless: bool,
    /// Loopback address for `--transport http`.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
    bind: String,
    /// Bearer token for HTTP. Falls back to the `AICAD_MCP_TOKEN` environment variable.
    #[arg(long, value_name = "BEARER")]
    token: Option<String>,
    /// Allowed browser `Origin`. Repeat the flag for more than one origin.
    #[arg(long = "allowed-origin", value_name = "ORIGIN")]
    allowed_origin: Vec<String>,
    /// Print tool schemas and exit. Only `openai` is supported.
    #[arg(long, value_name = "FORMAT")]
    export_tool_schema: Option<String>,
}

pub(crate) fn run(args: McpArgs) -> ExitCode {
    if let Some(format) = args.export_tool_schema.as_deref() {
        return export_schema(format);
    }
    match args.transport {
        McpTransport::Stdio => serve_stdio_transport(&args),
        McpTransport::Http => serve_http_transport(&args),
    }
}

fn serve_stdio_transport(args: &McpArgs) -> ExitCode {
    if !args.headless {
        if let Some(target) = crate::live_proxy::try_attach(&args.workspace) {
            return match crate::live_proxy::proxy_stdio(&target) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => fail(&HostError::new(
                    "E_IO",
                    format!("live-attach proxy failed: {err}"),
                )),
            };
        }
    }
    let Some(project) = args.project.as_deref() else {
        return fail(&HostError::new(
            "E400_INVALID_ARGUMENT",
            "--project is required when no desktop host is active",
        ));
    };
    let session = match runtime_host::open_or_create(&args.workspace, project) {
        Ok(session) => session,
        Err(err) => return fail(&err),
    };
    serve_stdio_session(&mut McpServer::from_session(session))
}

fn serve_http_transport(args: &McpArgs) -> ExitCode {
    let Some(project) = args.project.as_deref() else {
        return fail(&HostError::new(
            "E400_INVALID_ARGUMENT",
            "mcp HTTP transport requires --project",
        ));
    };
    let session = match runtime_host::open_or_create(&args.workspace, project) {
        Ok(session) => session,
        Err(err) => return fail(&err),
    };
    serve_http(&mut McpServer::from_session(session), args)
}

fn export_schema(format: &str) -> ExitCode {
    if !format.eq_ignore_ascii_case("openai") {
        return fail(&HostError::new(
            "E400_INVALID_ARGUMENT",
            format!("unknown tool schema `{format}`; supported formats: openai"),
        ));
    }
    match serde_json::to_string_pretty(&cad_mcp::export_openai_tool_schemas()) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&HostError::new(
            "E_IO",
            format!("cannot encode tool schema: {err}"),
        )),
    }
}

fn serve_stdio_session(server: &mut McpServer) -> ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match serve_stdio(server, stdin.lock(), stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(&HostError::new("E_IO", format!("stdio MCP failed: {err}"))),
    }
}

fn serve_http(server: &mut McpServer, args: &McpArgs) -> ExitCode {
    let addr: SocketAddr = match args.bind.parse() {
        Ok(addr) => addr,
        Err(err) => {
            return fail(&HostError::new(
                "E400_INVALID_ARGUMENT",
                format!("invalid --bind `{}`: {err}", args.bind),
            ));
        }
    };
    if !addr.ip().is_loopback() {
        return fail(&HostError::new(
            "E400_INVALID_ARGUMENT",
            "HTTP transport only binds to a loopback address such as 127.0.0.1:0",
        ));
    }
    let Some(token) = bearer_token(args.token.as_deref()) else {
        return fail(&HostError::new(
            "E400_INVALID_ARGUMENT",
            "HTTP transport requires --token or AICAD_MCP_TOKEN",
        ));
    };
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            return fail(&HostError::new(
                "E_IO",
                format!("cannot bind `{}`: {err}", args.bind),
            ));
        }
    };
    let local = match listener.local_addr() {
        Ok(local) => local,
        Err(err) => {
            return fail(&HostError::new(
                "E_IO",
                format!("bound listener has no address: {err}"),
            ));
        }
    };
    eprintln!("cad-cli mcp listening on http://{local}");
    let _ = std::io::stderr().flush();
    let config = HttpSecurityConfig {
        allowed_origins: args.allowed_origin.clone(),
        bearer_token: token,
    };
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                if let Err(err) = cad_mcp::serve_one_http_connection(&mut stream, server, &config) {
                    eprintln!("cad-cli mcp http connection: {err}");
                }
            }
            Err(err) => eprintln!("cad-cli mcp http accept: {err}"),
        }
    }
    ExitCode::SUCCESS
}

fn bearer_token(flag: Option<&str>) -> Option<String> {
    flag.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("AICAD_MCP_TOKEN")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
}

fn fail(err: &HostError) -> ExitCode {
    eprintln!("error: {}: {}", err.code, err.message);
    ExitCode::from(1)
}

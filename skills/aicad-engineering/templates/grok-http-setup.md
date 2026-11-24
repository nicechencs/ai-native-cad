# Grok / HTTP tool calling

`aicad-mcp` speaks MCP JSON-RPC 2.0. Streamable HTTP is the loopback guard in `crates/cad-mcp/src/http.rs`. OpenAI function definitions come from `export_openai_tool_schemas()` in `crates/cad-mcp/src/tools.rs`.

`cad-cli mcp` and `cad-cli doctor-agent` are registered subcommands. Stdio is the default transport. Loopback HTTP is for clients that cannot launch a process.

| Command | What it does |
| --- | --- |
| `cad-cli mcp --workspace .` | Live-Attach. Stdio server `aicad-mcp` (one JSON object per line). Mounts the desktop session when `.aicad-runtime.json` is live. |
| `cad-cli mcp --workspace . --project part.aicad` | Headless. Opens that project in this process when no desktop session is running. |
| `cad-cli mcp --transport http --bind 127.0.0.1:0 --project part.aicad --token <secret> --allowed-origin http://127.0.0.1` | `bind_streamable_http()` binds `127.0.0.1` and port `0` (ephemeral). It does not bind other interfaces. HTTP requires `--project`, `--token` (or `AICAD_MCP_TOKEN`), and at least one `--allowed-origin` (omitting `--allowed-origin` rejects every request with HTTP 403). |
| `cad-cli mcp --export-tool-schema openai` | `export_openai_tool_schemas()` returns eight `{ "type": "function", "function": { "name", "description", "parameters" } }` objects. `parameters` is the MCP `inputSchema`. |
| `cad-cli doctor-agent --json` | Prints the binary, `kernel.authoritative`, the tool-token budget, workspace write access, and `live_attach.status`. |

## Streamable HTTP

Start the loopback HTTP listener with both a bearer secret (`--token` or `AICAD_MCP_TOKEN`) and an explicit `--allowed-origin` allow list (repeat `--allowed-origin` for multiple origins; omitting it leaves the allow list empty and returns HTTP 403 for every request):

```sh
# Pass the bearer token explicitly:
cad-cli mcp --transport http --bind 127.0.0.1:0 --project part.aicad \
  --token "<secret>" --allowed-origin http://127.0.0.1

# Or supply the token via environment variable (PowerShell: $env:AICAD_MCP_TOKEN = "<secret>"):
AICAD_MCP_TOKEN="<secret>" cad-cli mcp --transport http --bind 127.0.0.1:0 \
  --project part.aicad --allowed-origin http://127.0.0.1
```

In Rust hosts:

1. Bind with `bind_streamable_http()`. Read the ephemeral port from the listener's local address.
2. Build `HttpSecurityConfig`: `bearer_token` is the shared secret, `allowed_origins` is an exact string list (for example `["http://127.0.0.1"]`).
3. Serve with `serve_one_http_connection`, `HttpSecurityConfig::serve_one_http_connection_with_hook`, or `handle_http_request`.

Every accepted request is `POST`. The body is one JSON-RPC object. Batches are rejected. The test request line is `POST /mcp`. The handler does not switch on the path. It validates headers before reading the body and requires both headers:

- `Authorization: Bearer <token>` — scheme compared without case sensitivity, token checked in constant time over SHA-256 digests. A missing, empty, or wrong token is HTTP 401 and `{"error":"unauthorized"}`.
- `Origin: <exact allow-list value>` — missing, blank, or unlisted is HTTP 403 and `{"error":"forbidden_origin"}`.

Limits and other statuses: headers above 64 KiB return 431 `{"error":"request_header_fields_too_large"}`. `Content-Length` above 16 MiB returns 413 `{"error":"payload_too_large"}`. An empty body is 400 `{"error":"empty_body"}`. A method other than `POST` is 405 `{"error":"method_not_allowed"}`.

```http
POST /mcp HTTP/1.1
Host: 127.0.0.1
Content-Type: application/json
Origin: http://127.0.0.1
Authorization: Bearer <token>
Content-Length: <bytes>

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"grok","version":"0"}}}
```

Accepted `protocolVersion` values are `2024-11-05`, `2025-03-26`, and `2025-06-18`. An unknown version is answered as `2025-03-26`. Methods: `initialize`, `ping`, `tools/list`, `tools/call`, `resources/list`, `resources/read`.

Call a tool with `tools/call`:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "tools/call",
  "params": {
    "name": "cad_session_context",
    "arguments": {}
  }
}
```

A tool error is still a JSON-RPC result. `isError` is true. The text payload is `{"code","message"}`.

## OpenAI / xAI schema

Load the eight functions from `export_openai_tool_schemas()` into the Grok or OpenAI tool list. Do not hand-write a ninth tool. Names, arguments, and resources are the playbook in `../SKILL.md`.

After `initialize`, `tools/list` must show only:

`cad_session_context`, `cad_inspect_and_slice`, `cad_query_geometry`, `cad_apply_commands`, `cad_sync_kcl_view`, `cad_verify_and_render`, `cad_branch_control`, `cad_export_artifacts`.

## Verify with `cad-cli doctor-agent --json`

Run this in the workspace before the first modeling call:

```text
cad-cli doctor-agent --json
```

Read the JSON. A clap "unrecognized subcommand" line is a failed launch, not a healthy report.

- `kernel.authoritative` is `true` only when the OCCT kernel is the backend. When it is `false`, L4 stays `UNKNOWN`.
- `live_attach.status` is `live` when a desktop host is attached. `absent` or `stale` means start the desktop app, or launch `cad-cli mcp` with `--project <path.aicad>`.

`status` is `ok` or `error`. An `error` report is a failed preflight. Fix the field it names before modeling.

`cargo test -p cad-mcp` locks the gateway checks below:

1. Server name `aicad-mcp`, protocol `2025-03-26`.
2. `tools/list` contains the eight names above and no others.
3. `export_openai_tool_schemas()` has eight `type: function` entries, each with a non-empty `description` and `parameters.additionalProperties: false`.
4. `resources/list` includes `aicad://schema/semantic-commands`, `aicad://stdlib/fasteners`, `aicad://stdlib/bearings`, `aicad://stdlib/iso286`, `aicad://stdlib/materials`, and `aicad://diagnostics/catalog`.
5. Loopback bind is `127.0.0.1` with port `0`. A request with no bearer returns 401 `unauthorized`. A request with a bearer and a foreign `Origin` returns 403 `forbidden_origin`.
6. An export path that leaves the workspace returns `E_PATH_ESCAPE`.

`cad-cli check --json` and `cad-cli build --json` check the CAD-DSL compiler. They do not open `aicad-mcp`.

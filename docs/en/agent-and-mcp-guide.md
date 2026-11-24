# AI Ecosystem, MCP Gateway & Python SDK Guide

**English** | [简体中文](../zh/agent-and-mcp-guide.md) | [Docs Index](../README.md)

AI-Native CAD lets **external general-purpose agents (Claude Code, OpenAI Codex, Cursor, xAI Grok, Antigravity)**, the **built-in `services/cad-agent`**, and the **official Python SDK (`aicad`)** drive the exact same `DocumentRuntime` and `Semantic CAD API` without forked glue logic.

---

## 1. Bring-Your-Own-Agent via MCP & CLI

Implemented in `crates/cad-mcp` and launched via `cad-cli mcp`, the gateway exposes **8 orthogonal MCP tools** (strictly bounded schema token budget) plus progressive **`aicad://` resources**.

### Step 1: Preflight the Workspace

```sh
cad-cli doctor-agent --json
```

Inspect two fields first:
- `kernel.authoritative`: `true` when native OCCT is loaded (L4 STEP round-trip can reach `PASS`); `false` on the pure-Rust fallback (L4 stays `UNKNOWN`).
- `live_attach.status`: `live` when the Tauri desktop host has published `.aicad-runtime.json`; `absent` or `stale` means launch in headless mode with `--project <path.aicad>` or start the desktop app.

### Three Runtime Modes

| Mode | Command | Best For |
| :--- | :--- | :--- |
| **Desktop Live-Attach** | `cad-cli mcp --workspace .` | Attaching to a live Tauri session, reading real-time viewport `TopoRef` selections, and pushing branch diffs to `Change Preview` |
| **Headless In-Process** | `cad-cli mcp --workspace . --project part.aicad` | Terminal workflows, Git worktrees, and CI—exits cleanly when `stdio` closes with zero orphan daemons |
| **Loopback Streamable HTTP** | `cad-cli mcp --transport http --bind 127.0.0.1:0 --project part.aicad --token "<secret>" --allowed-origin http://127.0.0.1` | HTTP tool-calling clients (e.g., Grok); requires Bearer token + exact `Origin` allow-list; supports `--export-tool-schema openai` |

Ready-to-copy client setups live in `skills/aicad-engineering/`:
- Engineering playbook: [skills/aicad-engineering/SKILL.md](../../skills/aicad-engineering/SKILL.md)
- Claude Code / Cursor templates: `skills/aicad-engineering/templates/claude-code.mcp.json` (Live-Attach) & `claude-code.headless.mcp.json` (Headless)
- Codex config template: `skills/aicad-engineering/templates/codex-config.toml`
- Grok / HTTP setup details: [skills/aicad-engineering/templates/grok-http-setup.md](../../skills/aicad-engineering/templates/grok-http-setup.md)

### 8 Orthogonal MCP Tools & CLI Parity

| MCP Tool | Equivalent CLI Command | Role |
| :--- | :--- | :--- |
| `cad_session_context` | `cad-cli session status` | Read `revision`, mode, active branch, viewport `TopoRef` selection, and `DesignContract` |
| `cad_inspect_and_slice` | `cad-cli project inspect` / `slice-context` | Document summary or minimal closed local context slice around focus nodes/faces |
| `cad_query_geometry` | `cad-cli query` | Filter faces/edges deterministically, run exact measurements, or `explain` parameter/topology lineage |
| `cad_apply_commands` | `cad-cli api apply` | Single structured write gate accepting atomic `SemanticCommand` batches with `expected_revision` |
| `cad_sync_kcl_view` | `cad-cli project emit-kcl` / sync | Read or update the `.kcl 2.0` view and commit the diff to a preview branch |
| `cad_verify_and_render` | `cad-cli verify` | Run 5-layer tri-state verification (`PASS`/`FAIL`/`UNKNOWN`) plus optional multi-view PNGs |
| `cad_branch_control` | `cad-cli project branch` | `list`, `diff`, `request_review`, `commit_preview`, `discard_preview`, `undo`, `redo` |
| `cad_export_artifacts` | `cad-cli build` | Export `STEP`, `3MF`, `DXF`, or `SVG` inside the workspace sandbox (`E_PATH_ESCAPE` blocks escapes) |

Progressive MCP resources: `aicad://schema/semantic-commands`, `aicad://schema/feature/<kind>`, `aicad://stdlib/fasteners`, `aicad://stdlib/bearings`, `aicad://stdlib/iso286`, `aicad://stdlib/materials`, and `aicad://diagnostics/catalog`.

---

## 2. Concurrency & Security Guardrails

- **Optimistic Revision Lock**: Every write carries `expected_revision`; concurrent edits return `E409_REVISION_CONFLICT` rather than silently overwriting user work.
- **Branch Sandbox by Default**: In `live_attached` mode, external writes target `agent/<client>/<session_id>` and cannot directly mutate `main` without human approval.
- **Untrusted Asset Label Isolation**: Imported STEP/IGES/mesh names are sanitized and isolated under `untrusted_asset_labels` to defend against indirect prompt injection.

---

## 3. Built-in Agent Service (`services/cad-agent`)

Built with zero third-party runtime dependencies on Python 3.12+, `services/cad-agent` provides `LLMSemanticPlanner`, `SemanticAgent`, `v2_context_adapter` (external mating references), and `BranchPatchExplorer` (Best-of-N candidate evaluation).

```sh
cargo build -p cad-cli --locked
python -m pip install -e services/cad-agent

# Configure OPENAI_API_KEY or GEMINI_API_KEY first
python -m cad_agent.cli \
  --semantic-v2 \
  --project part.aicad \
  --workspace-root . \
  --prompt "80 x 60 x 10 mm aluminum mounting plate with 4 M6 counterbored holes" \
  --json
```

See [services/cad-agent/README.md](../../services/cad-agent/README.md) for details.

---

## 4. Official Python SDK (`packages/python-sdk`)

Use the `aicad` Python SDK for algorithmic geometry, parametric batch workflows, preview branching, verification, and automated artifact export:

```sh
python -m pip install -e packages/python-sdk
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
python -X utf8 examples/developer-platform/02_parametric_commit_and_verify.py
python -X utf8 examples/developer-platform/03_branch_preview_and_promote.py
```

See [packages/python-sdk/README.md](../../packages/python-sdk/README.md) and [examples/developer-platform/README.md](../../examples/developer-platform/README.md).
